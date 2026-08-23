use crate::*;
use ahash::AHashMap;
use std::sync::Arc;

#[cfg(test)]
mod tests {

    use rue_query::QueryKey;

    use super::*;

    fn snapshot_with_file_id(file_id: u32, source: &str) -> SourceSnapshot {
        let file_id = FileId::new(file_id);
        let metadata = SourceMetadata::new(
            file_id,
            [(file_id, "/p/main.rue".to_owned())].into_iter().collect(),
            [(file_id, "main.rue".to_owned())].into_iter().collect(),
        )
        .unwrap();
        SourceSnapshot::new(metadata, vec![(file_id, Arc::new(source.to_owned()))]).unwrap()
    }

    fn wide_reached_program(functions: usize, leaf_value: i32) -> SourceSnapshot {
        assert!(functions > 0);
        let mut source = format!("fn f0() -> i32 {{ {leaf_value} }} ");
        for index in 1..functions {
            source.push_str(&format!("fn f{index}() -> i32 {{ f{}() }} ", index - 1));
        }
        source.push_str(&format!("fn main() -> i32 {{ f{}() }}", functions - 1));
        SourceSnapshot::single("<wide-backend-root>", source).unwrap()
    }

    #[test]
    fn backend_query_keys_share_exact_batch_and_memo_display_identities() {
        let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rooted = session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        let optimized = rooted.cfgs[0].optimized_cfg_key.clone();
        let cfg = optimized.cfg.clone();
        let codegen = crate::codegen_query::CodegenUnitQueryKey::new(
            optimized.clone(),
            options.target,
            rue_codegen::BackendArtifactRequest::default(),
            options.opt_level,
        );
        let object = crate::object_query::ObjectProjectionQueryKey::new(codegen.clone());

        for key in [
            cfg.stable_identity() == cfg.shared_stable_identity().as_ref(),
            optimized.stable_identity() == optimized.shared_stable_identity().as_ref(),
            codegen.stable_identity() == codegen.shared_stable_identity().as_ref(),
            object.stable_identity() == object.shared_stable_identity().as_ref(),
        ] {
            assert!(key, "owned and shared query display identities must agree");
        }

        let optimized_batch = crate::revisioned_query_database::OptimizedCfgBatchKey {
            keys: Arc::from([optimized.clone()]),
            generation: 0,
        };
        let codegen_batch = crate::revisioned_query_database::CodegenUnitBatchKey {
            keys: Arc::from([codegen.clone()]),
        };
        let object_batch = crate::revisioned_query_database::ObjectProjectionBatchKey {
            keys: Arc::from([object.clone()]),
        };
        assert_eq!(
            optimized_batch.stable_identity(),
            format!(
                "optimized-cfg-batch;units=1\u{1e}{};generation=0",
                optimized.shared_stable_identity()
            )
        );
        assert_eq!(
            codegen_batch.stable_identity(),
            format!(
                "codegen-unit-batch;units=1\u{1e}{}",
                codegen.shared_stable_identity()
            )
        );
        assert_eq!(
            object_batch.stable_identity(),
            format!(
                "object-projection-batch;units=1\u{1e}{}",
                object.shared_stable_identity()
            )
        );
    }

    #[cfg(unix)]
    fn execute_compiled_output(output: &CompileOutput, label: &str) -> std::process::Output {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_EXECUTABLE: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT_EXECUTABLE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rue-compiler-specialization-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::write(&path, &output.elf).expect("write linked Rue executable");
        let mut permissions = std::fs::metadata(&path)
            .expect("read linked Rue executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("make linked Rue executable runnable");
        #[cfg(target_os = "macos")]
        {
            let signing = std::process::Command::new("codesign")
                .args([
                    "-f",
                    "-s",
                    "-",
                    "--identifier",
                    "dev.rue-lang.compiler-test",
                    "--timestamp=none",
                ])
                .arg(&path)
                .output()
                .expect("run ad-hoc codesign for linked Rue executable");
            assert!(
                signing.status.success(),
                "codesign linked Rue executable: {}",
                String::from_utf8_lossy(&signing.stderr)
            );
        }
        let result = std::process::Command::new(&path).output();
        let cleanup = std::fs::remove_file(&path);
        cleanup.expect("remove linked Rue executable after execution");
        result.expect("execute linked Rue program")
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_self_enrollment_probe() {
        // This assertion is intentionally tiny: its purpose is to prove that
        // a newly added host-conditional test is picked up by the focused
        // platform target through its graph label, without a workflow edit.
        assert!(cfg!(unix));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_warm_specialization_with_strings_matches_fresh_link_and_execution() {
        let snapshot = SourceSnapshot::single(
            "<specialization-strings>",
            r#"
                fn choose(comptime result: i32) -> i32 {
                    let message: str = "hello";
                    @dbg(message);
                    result
                }

                fn main() -> i32 { choose(42) }
            "#,
        )
        .unwrap();
        let cold_options = CompileOptions::default();
        let optimized = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };

        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        warm_session.rooted_cfg(&cold_options).unwrap();
        let warm = warm_session.rooted_cfg(&optimized).unwrap();
        assert!(warm.string_domains().any(|strings| !strings.is_empty()));
        let warm_atoms = warm
            .functions()
            .iter()
            .flat_map(|function| function.record.local_atoms.iter())
            .collect::<Vec<_>>();
        assert_eq!(warm_atoms.len(), 1);
        assert_eq!(warm_atoms[0].content.as_ref(), "hello");

        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let fresh = fresh_session.rooted_cfg(&optimized).unwrap();
        let fresh_atoms = fresh
            .functions()
            .iter()
            .flat_map(|function| function.record.local_atoms.iter())
            .collect::<Vec<_>>();
        assert_eq!(warm_atoms, fresh_atoms);
        assert_eq!(
            format!("{:?}", warm.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(warm.type_pool_stats(), fresh.type_pool_stats());
        let named_types = |semantic: &RootedCfgOutput| {
            semantic
                .type_pools()
                .map(|pool| {
                    (
                        pool.all_struct_ids()
                            .into_iter()
                            .map(|id| format!("{:?}", pool.struct_def(id)))
                            .collect::<Vec<_>>(),
                        pool.all_enum_ids()
                            .into_iter()
                            .map(|id| format!("{:?}", pool.enum_def(id)))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(named_types(&warm), named_types(&fresh));
        assert_eq!(
            warm.string_domains().collect::<Vec<_>>(),
            fresh.string_domains().collect::<Vec<_>>()
        );
        assert_eq!(
            format!("{:?}", warm.warnings()),
            format!("{:?}", fresh.warnings())
        );

        let warm_output =
            crate::queries::compile_with_session(&mut warm_session, &snapshot, &optimized).unwrap();
        let fresh_output =
            crate::queries::compile_with_session(&mut fresh_session, &snapshot, &optimized)
                .unwrap();
        assert_eq!(
            format!("{:?}", warm_output.warnings),
            format!("{:?}", fresh_output.warnings)
        );

        let warm_execution = execute_compiled_output(&warm_output, "warm");
        let fresh_execution = execute_compiled_output(&fresh_output, "fresh");
        assert_eq!(
            warm_execution.status.code(),
            Some(42),
            "warm execution failed: {warm_execution:?}"
        );
        assert_eq!(warm_execution.stdout, b"hello\n");
        assert!(warm_execution.stderr.is_empty());
        assert_eq!(warm_execution.status.code(), fresh_execution.status.code());
        assert_eq!(warm_execution.stdout, fresh_execution.stdout);
        assert_eq!(warm_execution.stderr, fresh_execution.stderr);
    }

    /// ADR-0063 Phase 12's structural warm-edit gate. A body-only edit must
    /// recompute exactly that frontend cone and replace only its `CodegenUnit`;
    /// the deliberately fresh link still has to produce the same runnable
    /// executable as a fresh session for the edited source.
    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_warm_single_function_edit_recomputes_one_codegen_unit_then_fresh_links() {
        let before = SourceSnapshot::single(
            "<warm-single-function-edit>",
            "fn callee() -> i32 { 1 } fn main() -> i32 { callee() }",
        )
        .unwrap();
        let after = SourceSnapshot::single(
            "<warm-single-function-edit>",
            "fn callee() -> i32 { 2 } fn main() -> i32 { callee() }",
        )
        .unwrap();
        let options = CompileOptions::default();

        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut warm_session, &before, &options).unwrap();
        warm_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let warm =
            crate::queries::compile_with_session(&mut warm_session, &after, &options).unwrap();

        let metrics = warm.unstable_metrics();
        assert_eq!(metrics.parsed.lexer_invocations, 1);
        assert_eq!(metrics.parsed.parser_invocations, 1);
        assert_eq!(metrics.parsed.modules_reparsed, 1);
        assert_eq!(
            metrics.lowered.parser_invocations, 0,
            "the parsed successor reuses the canonical RIR lowering boundary"
        );
        assert_eq!(metrics.semantic.body.analyses_computed, 1);
        assert_eq!(metrics.semantic.body.analyses_reused, 1);
        assert_eq!(metrics.semantic.body.analyses_invalidated, 1);
        assert_eq!(metrics.semantic.cfg.cfg_builds_attempted, 1);
        assert_eq!(metrics.semantic.cfg.cfg_builds_succeeded, 1);

        assert_eq!(warm_session.rooted_cfg_executions().len(), 2);
        assert!(warm_session.rooted_cfg_executions().iter().any(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "callee")
                && *execution == rue_query::RequestExecution::Computed
        }));
        assert!(warm_session.rooted_cfg_executions().iter().any(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));

        assert_eq!(warm_session.codegen_executions().len(), 2);
        assert!(warm_session.codegen_executions().iter().any(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "callee")
                && *execution == rue_query::RequestExecution::Computed
        }));
        assert!(warm_session.codegen_executions().iter().any(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));

        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let fresh =
            crate::queries::compile_with_session(&mut fresh_session, &after, &options).unwrap();
        assert_eq!(warm.elf, fresh.elf);
        assert_eq!(warm.warnings, fresh.warnings);

        let execution = execute_compiled_output(&warm, "warm-single-function-edit");
        assert_eq!(
            execution.status.code(),
            Some(2),
            "freshly linked warm output did not run: {execution:?}"
        );
        assert!(execution.stdout.is_empty());
        assert!(execution.stderr.is_empty());
    }

    #[test]
    fn published_backend_root_keeps_wide_no_edit_and_single_edit_builds_exactly_warm() {
        const CHAIN_FUNCTIONS: usize = 33;
        const REACHED_FUNCTIONS: usize = CHAIN_FUNCTIONS + 1;
        let before = wide_reached_program(CHAIN_FUNCTIONS, 1);
        let after = wide_reached_program(CHAIN_FUNCTIONS, 2);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();

        session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        let cold_root = session.backend_root_metrics();
        assert_eq!(cold_root.functions, REACHED_FUNCTIONS, "{cold_root:?}");
        assert_eq!(cold_root.cfg_terminals, REACHED_FUNCTIONS, "{cold_root:?}");
        assert_eq!(
            cold_root.optimized_cfg_terminals, REACHED_FUNCTIONS,
            "{cold_root:?}"
        );
        assert_eq!(
            cold_root.codegen_unit_terminals, REACHED_FUNCTIONS,
            "{cold_root:?}"
        );

        let no_edit =
            crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        assert_eq!(session.rooted_cfg_executions().len(), REACHED_FUNCTIONS);
        assert!(
            session
                .rooted_cfg_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused),
            "{:?}",
            session.rooted_cfg_executions()
        );
        assert_eq!(session.codegen_executions().len(), REACHED_FUNCTIONS);
        assert!(
            session
                .codegen_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused),
            "{:?}",
            session.codegen_executions()
        );

        session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let warm = crate::queries::compile_with_session(&mut session, &after, &options).unwrap();
        assert_eq!(
            session
                .rooted_cfg_executions()
                .iter()
                .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
                .count(),
            1,
            "{:?}",
            session.rooted_cfg_executions()
        );
        assert_eq!(
            session
                .codegen_executions()
                .iter()
                .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
                .count(),
            1,
            "{:?}",
            session.codegen_executions()
        );
        let edited_root = session.backend_root_metrics();
        assert_eq!(edited_root.functions, REACHED_FUNCTIONS, "{edited_root:?}");

        let mut fresh = CompilerSession::new();
        fresh.update_for_presentation(&after).into_result().unwrap();
        let expected = crate::queries::compile_with_session(&mut fresh, &after, &options).unwrap();
        assert_eq!(warm.elf, expected.elf);
        assert_eq!(warm.warnings, expected.warnings);
        assert_ne!(no_edit.elf, warm.elf);
    }

    #[test]
    fn published_backend_root_releases_unreachable_functions_after_successful_handoff() {
        const CHAIN_FUNCTIONS: usize = 33;
        const REACHED_FUNCTIONS: usize = CHAIN_FUNCTIONS + 1;
        let before = wide_reached_program(CHAIN_FUNCTIONS, 1);
        let after =
            SourceSnapshot::single("<wide-backend-root>", "fn main() -> i32 { 7 }").unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();

        session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        let before_root = session.backend_root_metrics();
        assert_eq!(before_root.functions, REACHED_FUNCTIONS, "{before_root:?}");

        session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &after, &options).unwrap();
        let after_root = session.backend_root_metrics();
        assert_eq!(after_root.functions, 1, "{after_root:?}");
        assert_eq!(after_root.cfg_terminals, 1, "{after_root:?}");
        assert_eq!(after_root.optimized_cfg_terminals, 1, "{after_root:?}");
        assert_eq!(after_root.codegen_unit_terminals, 1, "{after_root:?}");
        assert_eq!(
            after_root.deletions - before_root.deletions,
            (REACHED_FUNCTIONS - 1) as u64,
            "{before_root:?}\n{after_root:?}"
        );

        let evictions_before_pressure = session.query_evictions_for_test();
        for value in 100..116 {
            let pressure = SourceSnapshot::single(
                "<wide-backend-root>",
                format!("fn main() -> i32 {{ {value} }}"),
            )
            .unwrap();
            session
                .update_for_presentation(&pressure)
                .into_result()
                .unwrap();
            crate::queries::compile_with_session(&mut session, &pressure, &options).unwrap();
        }
        assert!(
            session.query_evictions_for_test() > evictions_before_pressure,
            "the released chain must face real query-family eviction pressure"
        );

        session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        assert_eq!(session.rooted_cfg_executions().len(), REACHED_FUNCTIONS);
        assert!(
            session
                .rooted_cfg_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Computed),
            "released CFGs should recompute after re-entry: {:?}",
            session.rooted_cfg_executions()
        );
        assert_eq!(session.codegen_executions().len(), REACHED_FUNCTIONS);
        assert!(
            session
                .codegen_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Computed),
            "released codegen units should recompute after re-entry: {:?}",
            session.codegen_executions()
        );
    }

    #[test]
    fn failed_and_canceled_backend_collections_preserve_the_last_good_root() {
        let before =
            SourceSnapshot::single("<backend-root-control>", "fn main() -> i32 { 0 }").unwrap();
        let failing =
            SourceSnapshot::single("<backend-root-control>", "fn main() -> i32 { 1 }").unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::with_query_concurrency(4);
        session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        let last_good = session.backend_root_metrics();

        session
            .update_for_presentation(&failing)
            .into_result()
            .unwrap();
        crate::codegen_query::with_test_codegen_failure_injection(|| {
            assert!(
                session
                    .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                    .is_err()
            );
        });
        assert_eq!(session.backend_root_metrics(), last_good);

        let recovered =
            SourceSnapshot::single("<backend-root-control>", "fn main() -> i32 { 2 }").unwrap();
        session
            .update_for_presentation(&recovered)
            .into_result()
            .unwrap();
        let (owner, joiner) = session.exercise_codegen_schedule_for_test(&options, true);
        assert_eq!(owner, rue_query::RequestExecution::Computed);
        assert_eq!(joiner, rue_query::RequestExecution::Aborted);
        assert_eq!(session.backend_root_metrics(), last_good);

        crate::queries::compile_with_session(&mut session, &recovered, &options).unwrap();
        let recovered_root = session.backend_root_metrics();
        assert_eq!(recovered_root.functions, 1, "{recovered_root:?}");
        assert_eq!(recovered_root.publications, last_good.publications + 1);
    }

    #[test]
    fn failed_wide_batches_release_their_unpublished_child_cones_under_pressure() {
        const CHAIN_FUNCTIONS: usize = 33;
        // RUE-1262 ruling on what the pressure compiles buy. They are a witness
        // generator, not headroom and not sustained contention: a failed batch
        // that leaks pins onto children it never published keeps those
        // terminals `protected` in the eviction scan, so they survive at *any*
        // iteration count and the discriminating check is the re-entry
        // assertion below. The compiles need only guarantee that a genuinely
        // released terminal would have gone by then, so that "still present"
        // means "leaked" rather than "not yet pushed out".
        //
        // That is set by the family bound, not by the host. Eviction here is
        // driven by a per-family retained-*count* watermark, and
        // `compiler.cfg`, `compiler.optimized-cfg`, and `compiler.codegen-unit`
        // are all built at BODY_QUERY_MEMO_RETENTION = 8 against the
        // CHAIN_FUNCTIONS + 1 = 34 terminals one compile publishes into each.
        // One compile clears every watermark four times over; two are margin.
        // The byte and pin budgets are a separate mechanism and never bind at
        // their defaults, so nothing here depends on the allocator or platform.
        //
        // This was 16, inherited from the sibling pressure tests in this
        // module, whose iterations compile a one-function program and are
        // nearly free. This one compiles a 34-function chain per iteration.
        // Width is load-bearing — it is what makes the batch wide and puts
        // terminals-per-compile above the family bound — so the iteration
        // count is the knob to cut, not CHAIN_FUNCTIONS.
        const PRESSURE_COMPILES: i32 = 2;
        let options = CompileOptions::default();
        let last_good_source =
            SourceSnapshot::single("<backend-root-failure-pressure>", "fn main() -> i32 { 0 }")
                .unwrap();
        let failed_source = wide_reached_program(CHAIN_FUNCTIONS, 100);
        let mut session = CompilerSession::with_query_concurrency(4);

        session
            .update_for_presentation(&last_good_source)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &last_good_source, &options).unwrap();
        let last_good = session.backend_root_metrics();

        let fail = |session: &mut CompilerSession, source: &SourceSnapshot| {
            session
                .update_for_presentation(source)
                .into_result()
                .unwrap();
            crate::codegen_query::with_test_codegen_failure_injection(|| {
                assert!(
                    session
                        .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                        .is_err()
                );
            });
            assert_eq!(session.backend_root_metrics(), last_good);
        };

        fail(&mut session, &failed_source);
        let evictions_before_pressure = session.query_evictions_for_test();
        for value in 101..101 + PRESSURE_COMPILES {
            fail(&mut session, &wide_reached_program(CHAIN_FUNCTIONS, value));
        }
        assert!(
            session.query_evictions_for_test() > evictions_before_pressure,
            "the pressure compiles must actually reach eviction; without it the \
             re-entry assertion below cannot distinguish a released child cone \
             from one that is merely still sitting under its family's watermark"
        );

        // The discriminating check. `f0` is the only body that differs between
        // `failed_source` and the pressure programs, so it is the one member of
        // the failed batch's cone whose recomputation proves the cone was
        // released: had the failed batch kept its pins, `f0`'s CodegenUnit
        // terminal would have been protected from the eviction above and this
        // re-entry would report `Reused`.
        fail(&mut session, &failed_source);
        assert_eq!(
            session.codegen_executions().len(),
            1,
            "host projection stops at the first deterministic CodegenUnit failure"
        );
        let (identity, execution) = &session.codegen_executions()[0];
        assert!(
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "f0")
                && *execution == rue_query::RequestExecution::Computed,
            "the changed failed leaf must recompute after its batch cone is released: {:?}",
            session.codegen_executions()
        );
    }

    #[test]
    fn published_backend_root_explicitly_retains_accessor_raw_cfg_dependencies() {
        let snapshot = SourceSnapshot::single(
            "<backend-root-accessor>",
            "struct P { x: i64, fn value(borrow self) -> borrow i64 { yield self.x; } } \
             fn helper() -> i64 { 1 } \
             fn main() -> i32 { let p = P { x: 7 }; if p.value() + helper() == 8 { 0 } else { 1 } }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &snapshot, &options).unwrap();

        let root = session.backend_root_metrics();
        assert_eq!(
            root.functions, 2,
            "accessor has no standalone unit: {root:?}"
        );
        assert_eq!(root.optimized_cfg_terminals, 2, "{root:?}");
        assert_eq!(root.codegen_unit_terminals, 2, "{root:?}");
        assert_eq!(
            root.cfg_terminals, 3,
            "main's spliced accessor CFG must be retained explicitly: {root:?}"
        );

        let inspected = session.rooted_cfg(&options).unwrap();
        let accessor_raw_cfg = inspected
            .cfgs
            .iter()
            .find(|unit| {
                matches!(
                    &unit.function,
                    FunctionInstanceKey::Definition(definition) if definition.name() == "main"
                )
            })
            .and_then(|unit| unit.optimized_cfg_key.accessor_dependencies.first())
            .cloned()
            .expect("main's optimized CFG records the accessor raw-CFG dependency");
        drop(inspected);
        assert!(session.backend_cfg_key_is_retained(&accessor_raw_cfg));

        let evictions_before_pressure = session.query_evictions_for_test();
        for value in 100..116 {
            let pressure = SourceSnapshot::single(
                "<backend-root-accessor-pressure>",
                format!("fn main() -> i32 {{ {value} }}"),
            )
            .unwrap();
            session
                .update_for_presentation(&pressure)
                .into_result()
                .unwrap();
            drop(session.rooted_cfg(&options).unwrap());
        }
        assert!(
            session.query_evictions_for_test() > evictions_before_pressure,
            "the accessor dependency must face real raw-CFG eviction pressure"
        );
        assert!(
            session.backend_cfg_key_is_retained(&accessor_raw_cfg),
            "the published backend cone must keep the exact raw accessor CFG retained"
        );

        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &snapshot, &options).unwrap();
        assert!(
            session
                .rooted_cfg_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused),
            "the accessor-dependent optimized CFGs should stay reusable: {:?}",
            session.rooted_cfg_executions()
        );
        assert!(
            session
                .codegen_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused),
            "the accessor-dependent codegen units should stay reusable: {:?}",
            session.codegen_executions()
        );
    }

    #[test]
    fn accessor_optimization_keeps_the_published_raw_cfg_interner_immutable() {
        use lasso::Key as _;

        let snapshot = SourceSnapshot::single(
            "<accessor-interner-isolation>",
            "struct P { x: i64, fn value(borrow self) -> borrow i64 { yield self.x; } } \
             fn main() -> i32 { let p = P { x: 7 }; if p.value() == 7 { 0 } else { 1 } }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();

        let rooted = session.rooted_cfg(&options).unwrap();
        let main = rooted
            .cfgs
            .iter()
            .find(|unit| unit.definition_source_name() == Some("main"))
            .expect("main publishes one optimized CFG");
        let optimized = main.record.clone();
        let raw_key = main.optimized_cfg_key.cfg.clone();
        drop(rooted);

        let raw = session.raw_cfg_record_for_test(raw_key);
        assert!(
            !Arc::ptr_eq(&raw.interner, &optimized.interner),
            "accessor optimization must own an isolated symbol universe"
        );
        assert!(optimized.interner.len() >= raw.interner.len());
        for ordinal in 0..raw.interner.len() {
            let symbol = lasso::Spur::try_from_usize(ordinal).unwrap();
            assert_eq!(
                raw.interner.resolve(&symbol),
                optimized.interner.resolve(&symbol),
                "base CFG Spur {ordinal} changed during accessor isolation"
            );
        }
        let raw_len = raw.interner.len();
        let raw_utf8_bytes = raw.interner.utf8_bytes();
        let raw_charge = raw.frozen_interner_retained_charge_for_test();

        std::thread::scope(|scope| {
            for _ in 0..4 {
                let raw = raw.clone();
                scope.spawn(move || {
                    for ordinal in 0..raw.interner.len() {
                        let symbol = lasso::Spur::try_from_usize(ordinal).unwrap();
                        assert!(raw.interner.try_resolve(&symbol).is_some());
                    }
                });
            }
        });
        assert_eq!(raw.interner.len(), raw_len);
        assert_eq!(raw.interner.utf8_bytes(), raw_utf8_bytes);
        assert_eq!(raw.frozen_interner_retained_charge_for_test(), raw_charge);
    }

    #[test]
    fn ordinary_optimization_reuses_the_immutable_raw_cfg_interner() {
        let snapshot =
            SourceSnapshot::single("<ordinary-interner-sharing>", "fn main() -> i32 { 1 + 2 }")
                .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();

        let rooted = session.rooted_cfg(&options).unwrap();
        let main = rooted
            .cfgs
            .iter()
            .find(|unit| unit.definition_source_name() == Some("main"))
            .expect("main publishes one optimized CFG");
        let optimized = main.record.clone();
        let raw_key = main.optimized_cfg_key.cfg.clone();
        drop(rooted);
        let raw = session.raw_cfg_record_for_test(raw_key);

        assert!(
            Arc::ptr_eq(&raw.interner, &optimized.interner),
            "ordinary optimization must not copy an interner that cannot grow"
        );
        assert_eq!(
            raw.frozen_interner_retained_charge_for_test(),
            optimized.frozen_interner_retained_charge_for_test()
        );
    }

    /// Opt-in Phase 12 latency witness. This deliberately has no pass/fail
    /// timing threshold: it emits release-build samples and medians together
    /// with the exact structural work that makes the samples comparable.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn rue_1033_warm_single_function_latency_witness() {
        const SAMPLES: usize = 11;
        let options = CompileOptions::default();
        let snapshot = |value| {
            SourceSnapshot::single(
                "<rue-1033-latency>",
                format!("fn callee() -> i32 {{ {value} }} fn main() -> i32 {{ callee() }}"),
            )
            .unwrap()
        };
        let baseline = snapshot(1);

        let mut codegen_micros = Vec::with_capacity(SAMPLES);
        let mut runnable_micros = Vec::with_capacity(SAMPLES);
        for sample in 0..SAMPLES {
            // Give every sample the same two-revision history. Reusing one
            // session across the sample loop would make later samples observe
            // older retained revisions and cease to measure an identical edit.
            let mut session = CompilerSession::with_query_concurrency(4);
            session
                .update_for_presentation(&baseline)
                .into_result()
                .unwrap();
            crate::queries::compile_with_session(&mut session, &baseline, &options).unwrap();
            let edited = snapshot(sample as i32 + 2);
            let edit_start = std::time::Instant::now();
            session
                .update_for_presentation(&edited)
                .into_result()
                .unwrap();
            let rooted = session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let edit_to_codegen = edit_start.elapsed().as_micros();

            let parse = session.work().last_parse.clone();
            assert_eq!(parse.syntax.lexer_invocations, 1);
            assert_eq!(parse.syntax.parser_invocations, 1);
            assert_eq!(parse.modules_reparsed, 1);
            assert_eq!(rooted.work.body_analysis.body_analyses_computed, 1);
            assert_eq!(rooted.work.body_analysis.body_analyses_reused, 1);
            assert_eq!(rooted.work.body_analysis.body_analyses_invalidated, 1);
            assert_eq!(rooted.work.cfg.cfg_builds_attempted, 1);
            assert_eq!(rooted.work.cfg.cfg_builds_succeeded, 1);
            assert_eq!(
                session
                    .codegen_executions()
                    .iter()
                    .filter(|(_, execution)| {
                        *execution == rue_query::RequestExecution::Computed
                    })
                    .count(),
                1
            );
            assert_eq!(
                session
                    .codegen_executions()
                    .iter()
                    .filter(|(_, execution)| { *execution == rue_query::RequestExecution::Reused })
                    .count(),
                1
            );

            let image = crate::program_image_plan::ProgramImage::from_rooted(
                rooted.objects,
                rooted.exports,
                &options,
            )
            .unwrap();
            let output = image.fresh_link(&options, &rooted.warnings).unwrap();
            let edit_to_runnable = edit_start.elapsed().as_micros();
            let execution = execute_compiled_output(&output, &format!("rue-1033-latency-{sample}"));
            assert_eq!(execution.status.code(), Some(sample as i32 + 2));
            assert!(execution.stdout.is_empty());
            assert!(execution.stderr.is_empty());
            codegen_micros.push(edit_to_codegen);
            runnable_micros.push(edit_to_runnable);
        }

        let median = |samples: &mut Vec<u128>| {
            samples.sort_unstable();
            samples[samples.len() / 2]
        };
        let codegen_median = median(&mut codegen_micros);
        let runnable_median = median(&mut runnable_micros);
        eprintln!(
            "RUE-1033 warm single-function latency: target={} workers=4 samples={} \
             edit_to_codegen_unit_us={:?} median_edit_to_codegen_unit_us={} \
             edit_to_runnable_fresh_link_us={:?} median_edit_to_runnable_fresh_link_us={} \
             exact_work=lexer:1,parser:1,modules_reparsed:1,bodies_computed:1,bodies_reused:1,\
             bodies_invalidated:1,cfgs_computed:1,codegen_units_computed:1,codegen_units_reused:1,\
             fresh_links:1",
            options.target,
            SAMPLES,
            codegen_micros,
            codegen_median,
            runnable_micros,
            runnable_median,
        );
    }

    #[test]
    fn warm_unreachable_body_edit_reuses_every_rooted_downstream_terminal() {
        let before = SourceSnapshot::single(
            "<warm-unreachable-body-edit>",
            "fn dead() -> i32 { 1 } fn main() -> i32 { 7 }",
        )
        .unwrap();
        let after = SourceSnapshot::single(
            "<warm-unreachable-body-edit>",
            "fn dead() -> i32 { 2 } fn main() -> i32 { 7 }",
        )
        .unwrap();
        let options = CompileOptions::default();

        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        let cold =
            crate::queries::compile_with_session(&mut warm_session, &before, &options).unwrap();
        warm_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let warm =
            crate::queries::compile_with_session(&mut warm_session, &after, &options).unwrap();

        assert_eq!(warm_session.codegen_executions().len(), 1);
        assert!(warm_session.codegen_executions().iter().all(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));
        assert_eq!(warm.elf, cold.elf);
        assert_eq!(warm.warnings, cold.warnings);
        assert_eq!(warm.work.lowered, Default::default());
        assert_eq!(warm.work.semantic.body_analysis.body_analyses_computed, 0);
        assert_eq!(warm.work.semantic.body_analysis.body_analyses_reused, 1);
        assert_eq!(warm.work.semantic.cfg.cfg_builds_attempted, 0);
        assert_eq!(warm.work.semantic.cfg.cfg_reuses, 1);
        assert!(warm_session.rooted_cfg_executions().iter().all(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));

        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let fresh =
            crate::queries::compile_with_session(&mut fresh_session, &after, &options).unwrap();
        assert_eq!(warm.elf, fresh.elf);
        assert_eq!(warm.warnings, fresh.warnings);
    }

    #[test]
    fn unreachable_warning_reference_edit_changes_only_warning_projection() {
        let before = SourceSnapshot::single(
            "<warning-reference-edit>",
            "fn helper() -> i32 { 42 } fn dormant() -> i32 { helper() } fn main() -> i32 { 0 }",
        )
        .unwrap();
        let after = SourceSnapshot::single(
            "<warning-reference-edit>",
            "fn helper() -> i32 { 42 } fn dormant() -> i32 { 0 } fn main() -> i32 { 0 }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        let cold = crate::queries::compile_with_session(&mut session, &before, &options).unwrap();
        assert_eq!(cold.warnings.len(), 1);

        session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let warm = crate::queries::compile_with_session(&mut session, &after, &options).unwrap();
        assert_eq!(warm.elf, cold.elf);
        assert_eq!(warm.warnings.len(), 2);
        assert_eq!(warm.work.lowered, Default::default());
        assert_eq!(warm.work.semantic.body_analysis.body_analyses_computed, 0);
        assert_eq!(warm.work.semantic.body_analysis.body_analyses_reused, 1);
        assert_eq!(warm.work.semantic.cfg.cfg_builds_attempted, 0);
        assert_eq!(warm.work.semantic.cfg.cfg_reuses, 1);
        assert!(session.rooted_cfg_executions().iter().all(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));
        assert!(session.codegen_executions().iter().all(|(identity, execution)| {
            matches!(identity, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                && *execution == rue_query::RequestExecution::Reused
        }));
        assert_eq!(
            session
                .warning_reference_executions()
                .iter()
                .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
                .map(|(definition, _)| definition.name())
                .collect::<Vec<_>>(),
            vec!["dormant"]
        );
    }

    #[test]
    fn unreachable_body_local_import_alias_marks_exact_remote_helper_referenced() {
        let root = FileId::new(1);
        let library = FileId::new(2);
        let sources = [
            SourceView::new(
                "/p/main.rue",
                "fn dormant() -> i32 { let lib = @import(\"lib.rue\"); lib.helper() } fn main() -> i32 { 0 }",
                root,
            ),
            SourceView::new("/p/lib.rue", "fn helper() -> i32 { 42 }", library),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            root,
            AHashMap::from([
                (root, "main.rue".to_owned()),
                (library, "lib.rue".to_owned()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
        let output = test_compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        let warnings = output
            .warnings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert_eq!(warnings.len(), 1, "unexpected warnings: {warnings:?}");
        assert!(warnings[0].contains("unused function 'dormant'"));
        assert!(!warnings[0].contains("helper"));
    }

    #[test]
    fn reassigned_file_ids_reproject_rooted_warning_spans_and_report_truthful_work() {
        let source = "fn unused() -> i32 { 1 } fn main() -> i32 { 0 }";
        let before = snapshot_with_file_id(1, source);
        let reassigned = snapshot_with_file_id(17, source);
        let options = CompileOptions::default();
        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut warm_session, &before, &options).unwrap();
        warm_session
            .update_for_presentation(&reassigned)
            .into_result()
            .unwrap();
        let warm =
            crate::queries::compile_with_session(&mut warm_session, &reassigned, &options).unwrap();

        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&reassigned)
            .into_result()
            .unwrap();
        let fresh = crate::queries::compile_with_session(&mut fresh_session, &reassigned, &options)
            .unwrap();
        assert_eq!(warm.warnings, fresh.warnings);
        assert_eq!(warm.warnings.len(), 1);
        assert_eq!(
            warm.warnings[0]
                .span()
                .expect("unused warning has a span")
                .file_id,
            FileId::new(17)
        );
        assert_eq!(
            warm.work.semantic.body_analysis.body_analyses_computed
                + warm.work.semantic.body_analysis.body_analyses_reused,
            1,
            "work reports the one actual rooted body request"
        );
        assert_eq!(warm_session.rooted_cfg_executions().len(), 1);
        assert_eq!(
            warm.work.semantic.cfg.cfg_builds_attempted,
            warm_session
                .rooted_cfg_executions()
                .iter()
                .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
                .count(),
        );
        assert_eq!(
            warm.work.semantic.cfg.cfg_reuses,
            warm_session
                .rooted_cfg_executions()
                .iter()
                .filter(|(_, execution)| {
                    matches!(
                        execution,
                        rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                    )
                })
                .count(),
        );
        assert!(
            warm_session
                .warning_reference_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused)
        );
        assert_eq!(warm_session.codegen_executions().len(), 1);
    }

    #[test]
    fn rooted_compile_recrosses_the_current_import_error_gate() {
        let valid =
            SourceSnapshot::single("<rooted-import-gate>", "fn main() -> i32 { 0 }").unwrap();
        let invalid = SourceSnapshot::single(
            "<rooted-import-gate>",
            "const missing = @import(\"missing.rue\"); fn main() -> i32 { 0 }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&valid)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut session, &valid, &options).unwrap();
        session
            .update_for_presentation(&invalid)
            .into_result()
            .unwrap();
        let warm = crate::queries::compile_with_session(&mut session, &invalid, &options)
            .expect_err("the current unresolved import must reject rooted compilation");

        let mut fresh = CompilerSession::new();
        fresh
            .update_for_presentation(&invalid)
            .into_result()
            .unwrap();
        let fresh = crate::queries::compile_with_session(&mut fresh, &invalid, &options)
            .expect_err("fresh compilation rejects the same unresolved import");
        assert_eq!(warm, fresh);
    }

    #[test]
    fn rooted_compile_preserves_complete_duplicate_diagnostics() {
        let snapshot = SourceSnapshot::single(
            "<rooted-duplicate-diagnostics>",
            "fn dup() {} fn dup() {} struct clash {} fn clash() {} \
             enum kind {} struct kind {} struct record {} struct record {} \
             enum choice {} enum choice {} fn main() -> i32 { 0 }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let errors = crate::queries::compile_with_session(
            &mut session,
            &snapshot,
            &CompileOptions::default(),
        )
        .unwrap_err();
        assert_eq!(errors.len(), 5);
        assert!(matches!(
            errors.as_slice()[0].kind,
            ErrorKind::DuplicateFunctionDefinition { ref function_name } if function_name == "dup"
        ));
        assert!(matches!(
            errors.as_slice()[1].kind,
            ErrorKind::DuplicateFunctionDefinition { ref function_name } if function_name == "clash"
        ));
    }

    #[test]
    fn warm_cross_kind_reorder_matches_fresh_duplicate_diagnostic() {
        let before = SourceSnapshot::single(
            "<warm-duplicate-reorder>",
            "struct clash {} fn clash() {} fn main() -> i32 { 0 }",
        )
        .unwrap();
        let after = SourceSnapshot::single(
            "<warm-duplicate-reorder>",
            "fn clash() {} struct clash {} fn main() -> i32 { 0 }",
        )
        .unwrap();
        let options = CompileOptions::default();

        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&before)
            .into_result()
            .unwrap();
        crate::queries::compile_with_session(&mut warm_session, &before, &options).unwrap_err();
        warm_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let warm =
            crate::queries::compile_with_session(&mut warm_session, &after, &options).unwrap_err();

        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&after)
            .into_result()
            .unwrap();
        let fresh =
            crate::queries::compile_with_session(&mut fresh_session, &after, &options).unwrap_err();
        assert_eq!(warm, fresh);
    }

    /// The shared query-worker budget may change scheduling only. The complete
    /// fresh-link adapter must receive identical terminals and publish the
    /// same executable bytes with either serial or parallel codegen work.
    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_one_and_many_query_workers_produce_identical_linked_executables() {
        let snapshot = SourceSnapshot::single(
            "<worker-executable-determinism>",
            "fn a() -> i32 { 1 } fn b() -> i32 { 2 } fn c() -> i32 { 3 } fn main() -> i32 { a() + b() + c() }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let run = |workers| {
            let mut session = CompilerSession::with_query_concurrency(workers);
            crate::test_support::publish_test_snapshot(&mut session, &snapshot).unwrap();
            crate::queries::compile_with_session(&mut session, &snapshot, &options).unwrap()
        };

        let one = run(1);
        let many = run(4);
        assert_eq!(one.elf, many.elf);
        assert_eq!(one.warnings, many.warnings);
        for (label, output) in [("one-worker", &one), ("many-worker", &many)] {
            let execution = execute_compiled_output(output, label);
            assert_eq!(
                execution.status.code(),
                Some(6),
                "{label} linked output did not run: {execution:?}"
            );
            assert!(execution.stdout.is_empty());
            assert!(execution.stderr.is_empty());
        }
    }

    /// A registered CodegenUnit root donates the caller permit and dispatches
    /// independent children through the runtime's one structured worker budget.
    /// The gate makes overlap (or its absence) deterministic rather than timing
    /// dependent.
    #[test]
    fn codegen_root_batch_overlaps_with_many_workers_and_is_serial_with_one() {
        let snapshot = SourceSnapshot::single(
            "<backend-batch-overlap>",
            "fn a() -> i32 { 1 } fn b() -> i32 { 2 } fn main() -> i32 { a() + b() }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let run = |workers, gated_children, rendezvous| {
            let mut session = CompilerSession::with_query_concurrency(workers);
            session
                .update_for_presentation(&snapshot)
                .into_result()
                .unwrap();
            session.exercise_codegen_batch_overlap_for_test(&options, gated_children, rendezvous)
        };

        assert_eq!(run(1, 2, false), (1, 2));
        assert_eq!(run(4, 2, true), (2, 2));
    }

    /// The batch memo bound is intentionally smaller than this root. Retaining
    /// only the batch terminal without its exact child leases would evict and
    /// recompute early CodegenUnits during final backend-root publication.
    #[test]
    fn codegen_root_batch_retains_more_than_the_child_memo_bound_until_publication() {
        let snapshot = SourceSnapshot::single(
            "<backend-batch-retention>",
            "fn f00() -> i32 { 0 } fn f01() -> i32 { 1 } fn f02() -> i32 { 2 } \
             fn f03() -> i32 { 3 } fn f04() -> i32 { 4 } fn f05() -> i32 { 5 } \
             fn f06() -> i32 { 6 } fn f07() -> i32 { 7 } fn f08() -> i32 { 8 } \
             fn f09() -> i32 { 9 } fn f10() -> i32 { 10 } fn f11() -> i32 { 11 } \
             fn f12() -> i32 { 12 } fn f13() -> i32 { 13 } fn f14() -> i32 { 14 } \
             fn f15() -> i32 { 15 } \
             fn main() -> i32 { f00() + f01() + f02() + f03() + f04() + f05() + \
                 f06() + f07() + f08() + f09() + f10() + f11() + f12() + f13() + \
                 f14() + f15() }",
        )
        .unwrap();
        let mut session = CompilerSession::with_query_concurrency(4);
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();

        let (peak, evaluations) = session.exercise_codegen_batch_overlap_for_test(
            &CompileOptions::default(),
            usize::MAX,
            false,
        );

        assert!(peak > 0);
        assert_eq!(
            evaluations, 17,
            "publication must not recompute CodegenUnits"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_backend_batches_preserve_all_one_and_many_worker_projections() {
        let snapshot = SourceSnapshot::single(
            "<backend-batch-determinism>",
            "fn a() -> i32 { 1 } fn b() -> i32 { 2 } fn c() -> i32 { 3 } \
             fn main() -> i32 { let unused = 9; a() + b() + c() }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let run = |workers| {
            let mut session = CompilerSession::with_query_concurrency(workers);
            session
                .update_for_presentation(&snapshot)
                .into_result()
                .unwrap();
            let rooted = session
                .rooted_codegen(
                    &options,
                    rue_codegen::BackendArtifactRequest {
                        asm: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            let cfgs = rooted
                .cfgs
                .iter()
                .map(|cfg| {
                    (
                        format!("{:?}", cfg.function),
                        format!("{:?}", cfg.record.cfg),
                        cfg.record.codegen.defined_symbol.to_string(),
                    )
                })
                .collect::<Vec<_>>();
            let codegen_units = rooted
                .units
                .iter()
                .map(|unit| (format!("{:?}", unit.function), format!("{:?}", unit.unit)))
                .collect::<Vec<_>>();
            let diagnostics = rooted
                .warnings
                .iter()
                .map(|warning| (warning.to_string(), format!("{:?}", warning.diagnostic())))
                .collect::<Vec<_>>();
            let image = crate::program_image_plan::ProgramImage::from_rooted(
                rooted.objects,
                rooted.exports,
                &options,
            )
            .unwrap();
            let plan = image.plan.clone();
            let objects = image.fresh_objects(&options).unwrap();
            let executable = image.fresh_link(&options, &rooted.warnings).unwrap().elf;
            (cfgs, codegen_units, diagnostics, plan, objects, executable)
        };

        assert_eq!(run(1), run(4));
    }

    /// Opt-in cold and broad-invalidation backend wall-time witness. It has no
    /// timing threshold: samples include exact child counts so noisy hosts do
    /// not turn a performance observation into a correctness gate.
    #[test]
    #[ignore]
    fn rue_1228_backend_batch_latency_witness() {
        const FUNCTIONS: usize = 64;
        const SAMPLES: usize = 7;
        let options = CompileOptions::default();
        let snapshot = |salt: usize| {
            let mut source = String::new();
            for index in 0..FUNCTIONS {
                source.push_str(&format!("fn f{index}() -> i32 {{ {} }} ", index + salt));
            }
            source.push_str("fn main() -> i32 { ");
            for index in 0..FUNCTIONS {
                if index != 0 {
                    source.push_str(" + ");
                }
                source.push_str(&format!("f{index}()"));
            }
            source.push_str(" }");
            SourceSnapshot::single("<rue-1228-backend-latency>", source).unwrap()
        };

        let measure = |workers| {
            let mut cold = Vec::with_capacity(SAMPLES);
            let mut broad = Vec::with_capacity(SAMPLES);
            for sample in 0..SAMPLES {
                let before = snapshot(sample * 2);
                let after = snapshot(sample * 2 + 1);
                let mut session = CompilerSession::with_query_concurrency(workers);
                session
                    .update_for_presentation(&before)
                    .into_result()
                    .unwrap();
                let start = std::time::Instant::now();
                session
                    .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                    .unwrap();
                cold.push(start.elapsed().as_micros());

                session
                    .update_for_presentation(&after)
                    .into_result()
                    .unwrap();
                let start = std::time::Instant::now();
                session
                    .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                    .unwrap();
                broad.push(start.elapsed().as_micros());
                assert_eq!(
                    session
                        .codegen_executions()
                        .iter()
                        .filter(|(_, execution)| {
                            *execution == rue_query::RequestExecution::Computed
                        })
                        .count(),
                    FUNCTIONS,
                    "every independently edited callee CodegenUnit recomputes"
                );
            }
            cold.sort_unstable();
            broad.sort_unstable();
            (cold, broad)
        };

        let (one_cold, one_broad) = measure(1);
        let (many_cold, many_broad) = measure(4);
        eprintln!(
            "RUE-1228 backend batch latency: functions={} samples={} \
             workers=1 cold_us={:?} cold_median_us={} broad_us={:?} broad_median_us={} \
             workers=4 cold_us={:?} cold_median_us={} broad_us={:?} broad_median_us={}",
            FUNCTIONS,
            SAMPLES,
            one_cold,
            one_cold[SAMPLES / 2],
            one_broad,
            one_broad[SAMPLES / 2],
            many_cold,
            many_cold[SAMPLES / 2],
            many_broad,
            many_broad[SAMPLES / 2],
        );
    }

    #[test]
    fn retained_endpoint_capabilities_stop_and_continue_at_canonical_boundaries() {
        let snapshot = SourceSnapshot::single(
            "<retained-endpoints>",
            "fn helper() -> i32 { 41 } fn main() -> i32 { helper() + 1 }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut session)
            .unwrap();

        let codegen = crate::unstable::codegen_ready(&mut session, &options).unwrap();
        assert_eq!(session.codegen_collections(), 2);
        assert!(
            session.object_projection_executions().is_empty(),
            "codegen-ready must not begin object projection"
        );
        assert_eq!(session.object_projection_collections(), 0);

        let objects = crate::unstable::objects_ready(&mut session, codegen).unwrap();
        assert_eq!(session.object_projection_collections(), 2);
        let retained = crate::unstable::runnable_ready(&mut session, objects).unwrap();

        let mut fresh = CompilerSession::new();
        crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut fresh)
            .unwrap();
        let oracle = fresh.executable_in_compile_scope(&options).unwrap();
        assert_eq!(retained.elf, oracle.elf);
        assert_eq!(retained.warnings, oracle.warnings);
    }

    #[test]
    fn retained_endpoint_capabilities_reject_another_session_or_revision() {
        let snapshot =
            SourceSnapshot::single("<retained-endpoint-owner>", "fn main() -> i32 { 42 }").unwrap();
        let options = CompileOptions::default();

        let mut first = CompilerSession::new();
        crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut first)
            .unwrap();
        let wrong_owner = crate::unstable::codegen_ready(&mut first, &options).unwrap();

        let mut second = CompilerSession::new();
        crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut second)
            .unwrap();
        let wrong_owner_error = match crate::unstable::objects_ready(&mut second, wrong_owner) {
            Err(errors) => errors,
            Ok(_) => panic!("another session accepted a foreign endpoint capability"),
        };
        assert!(
            wrong_owner_error
                .to_string()
                .contains("another compiler session")
        );

        let stale = crate::unstable::codegen_ready(&mut first, &options).unwrap();
        crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut first)
            .unwrap();
        let stale_error = match crate::unstable::objects_ready(&mut first, stale) {
            Err(errors) => errors,
            Ok(_) => panic!("a newer revision accepted a stale endpoint capability"),
        };
        assert!(stale_error.to_string().contains("stale"));
    }

    #[test]
    fn retained_object_projection_is_local_bounded_and_released_with_the_backend_root() {
        const FUNCTIONS: usize = 32;
        let options = CompileOptions::default();
        let snapshot = |edited: Option<usize>, count: usize| {
            let mut source = String::new();
            for index in 0..count {
                let value = if edited == Some(index) { 99 } else { index };
                source.push_str(&format!("fn f{index}() -> i32 {{ {value} }} "));
            }
            source.push_str("fn main() -> i32 { ");
            for index in 0..count {
                if index != 0 {
                    source.push_str(" + ");
                }
                source.push_str(&format!("f{index}()"));
            }
            source.push_str(" }");
            SourceSnapshot::single("<retained-object-projection>", source).unwrap()
        };
        let computed = |session: &CompilerSession| {
            session
                .object_projection_executions()
                .iter()
                .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
                .count()
        };

        let initial = snapshot(None, FUNCTIONS);
        let mut session = CompilerSession::with_query_concurrency(4);
        session
            .update_for_presentation(&initial)
            .into_result()
            .unwrap();
        let cold = session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        let removed_object_key = cold
            .cfgs
            .iter()
            .find(|cfg| format!("{:?}", cfg.function).contains("f17"))
            .map(|cfg| {
                crate::object_query::ObjectProjectionQueryKey::new(
                    crate::codegen_query::CodegenUnitQueryKey::new(
                        cfg.optimized_cfg_key.clone(),
                        options.target,
                        rue_codegen::BackendArtifactRequest::default(),
                        options.opt_level,
                    ),
                )
            })
            .unwrap();
        assert!(session.object_projection_key_is_retained(&removed_object_key));
        assert_eq!(computed(&session), FUNCTIONS + 1);
        assert_eq!(session.object_projection_collections(), FUNCTIONS + 1);
        let canonical_objects = cold
            .units
            .iter()
            .map(|unit| crate::backend::project_backend_object(&unit.unit, options.target).unwrap())
            .collect::<Vec<_>>();
        let cold_image = crate::program_image_plan::ProgramImage::from_rooted(
            cold.objects,
            cold.exports,
            &options,
        )
        .unwrap();
        assert_eq!(
            cold_image.fresh_objects(&options).unwrap(),
            canonical_objects
        );

        let warm = session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        assert_eq!(computed(&session), 0, "a no-edit build reprojects no units");
        let warm_plan = crate::program_image_plan::ProgramImage::from_rooted(
            warm.objects,
            warm.exports,
            &options,
        )
        .unwrap()
        .plan;

        let edited = snapshot(Some(17), FUNCTIONS);
        session
            .update_for_presentation(&edited)
            .into_result()
            .unwrap();
        let changed = session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        assert_eq!(computed(&session), 1, "only f17 has new object content");
        let changed_plan = crate::program_image_plan::ProgramImage::from_rooted(
            changed.objects,
            changed.exports,
            &options,
        )
        .unwrap()
        .plan;
        let delta = changed_plan.delta_from(&warm_plan).unwrap();
        assert!(delta.added.is_empty(), "{delta:?}");
        assert_eq!(delta.changed.len(), 1, "{delta:?}");
        assert!(delta.removed.is_empty(), "{delta:?}");
        assert!(
            format!("{:?}", delta.changed[0].function).contains("f17"),
            "{delta:?}"
        );

        let reduced = snapshot(None, 1);
        session
            .update_for_presentation(&reduced)
            .into_result()
            .unwrap();
        session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        let root = session.backend_root_metrics();
        assert_eq!(root.functions, 2, "{root:?}");
        assert_eq!(root.object_projection_terminals, 2, "{root:?}");
        assert_eq!(root.deletions, FUNCTIONS as u64 - 1, "{root:?}");
        let retention = session.unstable_metrics().retention();
        assert!(retention.retained_bytes > 0);
        assert!(
            retention.retained_bytes <= retention.retained_byte_budget,
            "{retention:?}"
        );

        let mut pressure_source = String::new();
        for index in 0..70 {
            pressure_source.push_str(&format!("fn g{index}() -> i32 {{ {index} }} "));
        }
        pressure_source.push_str("fn main() -> i32 { ");
        for index in 0..70 {
            if index != 0 {
                pressure_source.push_str(" + ");
            }
            pressure_source.push_str(&format!("g{index}()"));
        }
        pressure_source.push_str(" }");
        let pressure =
            SourceSnapshot::single("<retained-object-projection>", pressure_source).unwrap();
        session
            .update_for_presentation(&pressure)
            .into_result()
            .unwrap();
        session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        assert!(
            !session.object_projection_key_is_retained(&removed_object_key),
            "an object absent from the published root is evictable under pressure"
        );

        session
            .update_for_presentation(&initial)
            .into_result()
            .unwrap();
        session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        assert!(
            session
                .object_projection_executions()
                .iter()
                .any(|(function, execution)| {
                    format!("{function:?}").contains("f17")
                        && *execution == rue_query::RequestExecution::Computed
                })
        );
    }

    #[cfg(unix)]
    fn assert_scheduled_codegen_matches_fresh_link(cancel_joiner: bool) {
        let snapshot =
            SourceSnapshot::single("<scheduled-codegen-executable>", "fn main() -> i32 { 42 }")
                .unwrap();
        let options = CompileOptions::default();
        let mut scheduled = CompilerSession::with_query_concurrency(2);
        scheduled
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let (owner, joiner) = scheduled.exercise_codegen_schedule_for_test(&options, cancel_joiner);
        assert_eq!(owner, rue_query::RequestExecution::Computed);
        assert_eq!(
            joiner,
            if cancel_joiner {
                rue_query::RequestExecution::Aborted
            } else {
                rue_query::RequestExecution::Joined
            }
        );

        // The ordinary compiler adapter must consume the terminal produced by
        // that schedule, collect the image plan, and fresh-link it. No test
        // helper constructs a CodegenUnit or executable directly.
        let scheduled_output =
            crate::queries::compile_with_session(&mut scheduled, &snapshot, &options).unwrap();
        assert!(
            scheduled
                .codegen_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused)
        );

        let mut fresh = CompilerSession::new();
        fresh
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let fresh_output =
            crate::queries::compile_with_session(&mut fresh, &snapshot, &options).unwrap();
        assert_eq!(scheduled_output.elf, fresh_output.elf);
        assert_eq!(scheduled_output.warnings, fresh_output.warnings);
        let execution = execute_compiled_output(
            &scheduled_output,
            if cancel_joiner {
                "canceled-codegen-schedule"
            } else {
                "joined-codegen-schedule"
            },
        );
        assert_eq!(execution.status.code(), Some(42), "{execution:?}");
        assert!(execution.stdout.is_empty());
        assert!(execution.stderr.is_empty());
    }

    /// A joined CodegenUnit request must be observationally equivalent to a
    /// fresh execution through final executable bytes.
    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_joined_codegen_schedule_matches_fresh_linked_executable() {
        assert_scheduled_codegen_matches_fresh_link(false);
    }

    /// Canceling one joined waiter must neither cancel nor corrupt the live
    /// CodegenUnit owner; the owner's terminal must remain usable through the
    /// ordinary fresh-link adapter and match a fresh compiler session.
    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_canceled_codegen_waiter_schedule_matches_fresh_linked_executable() {
        assert_scheduled_codegen_matches_fresh_link(true);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_named_const_strings_keep_stable_atoms_across_warm_reorder_and_codegen() {
        let first = SourceSnapshot::single(
            "<named-const-strings>",
            r#"
                const MESSAGE: str = "hello";

                fn helper() -> i32 {
                    @dbg(MESSAGE);
                    @dbg(MESSAGE);
                    7
                }

                fn main() -> i32 { helper() }
            "#,
        )
        .unwrap();
        let reordered = SourceSnapshot::single(
            "<named-const-strings>",
            r#"
                const MESSAGE: str = "hello";

                // Shift positions and reverse the sibling declarations.
                fn main() -> i32 { helper() }

                fn helper() -> i32 {
                    @dbg(MESSAGE);
                    @dbg(MESSAGE);
                    7
                }
            "#,
        )
        .unwrap();
        let optimized = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };

        let mut warm_session = CompilerSession::new();
        warm_session
            .update_for_presentation(&first)
            .into_result()
            .unwrap();
        let cold = warm_session.rooted_cfg(&optimized).unwrap();
        let cold_atoms = cold
            .functions()
            .iter()
            .flat_map(|function| function.record.local_atoms.iter())
            .collect::<Vec<_>>();
        assert_eq!(cold_atoms.len(), 2);
        assert!(cold_atoms.iter().all(|atom| {
            atom.identity.kind == rue_air::LocalAtomKind::ReadOnlyData
                && atom.content.as_ref() == "hello"
        }));
        assert_ne!(
            cold_atoms[0].identity.anchor, cold_atoms[1].identity.anchor,
            "duplicate-content const uses retain occurrence identity"
        );

        warm_session
            .update_for_presentation(&reordered)
            .into_result()
            .unwrap();
        let warm = warm_session.rooted_cfg(&optimized).unwrap();
        let warm_atoms = warm
            .functions()
            .iter()
            .flat_map(|function| {
                function
                    .record
                    .local_atoms
                    .iter()
                    .map(|atom| (atom, function.record.strings.as_ref()))
            })
            .collect::<Vec<_>>();
        assert_eq!(warm_atoms.len(), 2);
        assert!(warm_atoms.iter().all(|(atom, strings)| {
            atom.identity.kind == rue_air::LocalAtomKind::ReadOnlyData
                && strings.get(atom.dense_id as usize).map(String::as_str)
                    == Some(atom.content.as_ref())
        }));

        let warm_output =
            crate::queries::compile_with_session(&mut warm_session, &reordered, &optimized)
                .unwrap();
        let mut fresh_session = CompilerSession::new();
        fresh_session
            .update_for_presentation(&reordered)
            .into_result()
            .unwrap();
        let fresh_output =
            crate::queries::compile_with_session(&mut fresh_session, &reordered, &optimized)
                .unwrap();
        let warm_execution = execute_compiled_output(&warm_output, "named-const-warm");
        let fresh_execution = execute_compiled_output(&fresh_output, "named-const-fresh");
        assert_eq!(warm_execution.status.code(), Some(7));
        assert_eq!(warm_execution.stdout, b"hello\nhello\n");
        assert_eq!(warm_execution.status.code(), fresh_execution.status.code());
        assert_eq!(warm_execution.stdout, fresh_execution.stdout);
        assert_eq!(warm_execution.stderr, fresh_execution.stderr);
    }

    #[test]
    fn source_token_volume_survives_an_exact_zero_parse_update() {
        let snapshot =
            SourceSnapshot::single("/project/main.rue", "fn main() -> i32 { 42 }").unwrap();
        let mut session = CompilerSession::new();
        let first = session.update_for_presentation(&snapshot);
        let expected_tokens = first.work().syntax.tokens;
        first.into_result().unwrap();
        let exact = session.update_for_presentation(&snapshot);
        assert_eq!(exact.work().syntax.parser_invocations, 0);
        exact.into_result().unwrap();

        let output = crate::queries::compile_with_session(
            &mut session,
            &snapshot,
            &CompileOptions::default(),
        )
        .unwrap();
        assert_eq!(output.work.parsed.syntax.parser_invocations, 0);
        assert_eq!(output.source_stats.tokens, expected_tokens);
    }

    #[test]
    fn diagnostic_attempt_key_includes_presentation_order() {
        let first = FileId::new(1);
        let second = FileId::new(2);
        let physical = AHashMap::from([
            (first, "/project/first.rue".to_owned()),
            (second, "/project/second.rue".to_owned()),
        ]);
        let logical = AHashMap::from([
            (first, "first.rue".to_owned()),
            (second, "second.rue".to_owned()),
        ]);
        let metadata = SourceMetadata::new(first, physical, logical).unwrap();
        let first_text = Arc::new("fn first( {".to_owned());
        let second_text = Arc::new("fn second( {".to_owned());
        let forward = SourceSnapshot::new(
            metadata.clone(),
            vec![(first, first_text.clone()), (second, second_text.clone())],
        )
        .unwrap();
        let reverse =
            SourceSnapshot::new(metadata, vec![(second, second_text), (first, first_text)])
                .unwrap();
        assert_eq!(forward.source_revision(), reverse.source_revision());

        let mut session = CompilerSession::new();
        let forward_attempt = session.update_for_presentation(&forward);
        assert_eq!(
            forward_attempt
                .diagnostics()
                .errors()
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            [first, second]
        );
        let forward_diagnostics = forward_attempt.diagnostics().clone();

        let reverse_attempt = session.update_for_presentation(&reverse);
        assert_eq!(
            reverse_attempt
                .diagnostics()
                .errors()
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            [second, first]
        );
        assert!(!Arc::ptr_eq(
            &forward_diagnostics,
            reverse_attempt.diagnostics()
        ));
        let reverse_diagnostics = reverse_attempt.diagnostics().clone();

        let canonical_attempt = session.update(&reverse);
        assert_eq!(
            canonical_attempt
                .diagnostics()
                .errors()
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            [first, second]
        );
        assert!(!Arc::ptr_eq(
            &reverse_diagnostics,
            canonical_attempt.diagnostics()
        ));
        assert_eq!(session.work().diagnostic_publications, 3);

        let repeated_reverse = session.update_for_presentation(&reverse);
        assert!(Arc::ptr_eq(
            &reverse_diagnostics,
            repeated_reverse.diagnostics()
        ));
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&reverse, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &reverse_diagnostics
        ));
        assert_eq!(session.work().diagnostic_publications, 3);
    }

    #[test]
    fn canonical_std_strbuf_identity_survives_qualified_and_aliased_lookup() {
        let context = ImportDiscoveryContext::new(1, "/project", Some("/sdk"), "test")
            .expect("discovery context should be valid");
        let mut assembler = DiscoverySourceAssembler::new(
            context,
            "/project/main.rue",
            "/project/main.rue",
            PhysicalFileIdentity::new(1, 1),
            FileMetadataFingerprint::new(1, 1, 1),
            Arc::new(
                r#"
                    const std = @import("std/_std.rue");
                    const other = @import("other.rue");
                    const Qualified = std.strbuf.StrBuf;
                    const Alias = Qualified;
                    fn qualified(value: std.strbuf.StrBuf) -> std.strbuf.StrBuf { value }
                    fn aliased(value: Alias) -> Alias { value }
                    fn ordinary(value: other.StrBuf) -> i32 { value.value }
                    fn main() -> i32 {
                        let qualified_value: Qualified = "q";
                        let _qualified_result = qualified(qualified_value);
                        let aliased_value: Alias = "a";
                        let _aliased_result = aliased(aliased_value);
                        let ordinary_value = other.StrBuf { value: 7 };
                        ordinary(ordinary_value)
                    }
                "#
                .to_owned(),
            ),
        )
        .unwrap();
        assembler
            .add_explicit(
                "/sdk/_std.rue",
                "/sdk/_std.rue",
                PhysicalFileIdentity::new(2, 2),
                FileMetadataFingerprint::new(2, 2, 2),
                Arc::new("pub const strbuf = @import(\"strbuf.rue\");".to_owned()),
            )
            .unwrap();
        assembler
            .add_explicit(
                "/sdk/strbuf.rue",
                "/sdk/strbuf.rue",
                PhysicalFileIdentity::new(3, 3),
                FileMetadataFingerprint::new(3, 3, 3),
                Arc::new(
                    r#"
                    pub struct StrBuf {
                        buf: ptr mut u8,
                        len: u64,
                        cap: u64,
                        fn len(borrow self) -> u64 { self.len }
                    }
                    drop fn StrBuf(self) {}
                "#
                    .to_owned(),
                ),
            )
            .unwrap();
        assembler
            .add_explicit(
                "/project/other.rue",
                "/project/other.rue",
                PhysicalFileIdentity::new(4, 4),
                FileMetadataFingerprint::new(4, 4, 4),
                Arc::new("pub struct StrBuf { value: i32 }".to_owned()),
            )
            .unwrap();
        let snapshot = assembler.snapshot().unwrap();

        let options = CompileOptions::default();
        let (_rir, semantic, _) = test_frontend_snapshot(&snapshot, &options)
            .expect("qualified and aliased canonical StrBuf references should compile");
        assert!(semantic.type_pools().any(|pool| {
            pool.all_struct_ids()
                .any(|id| pool.struct_lang_item(id) == Some(rue_air::LangItem::StrBuf))
        }));
        assert!(semantic.type_pools().any(|pool| {
            pool.all_struct_ids().any(|id| {
                &*pool.struct_def(id).name == "StrBuf"
                    && !pool.struct_def(id).is_builtin
                    && pool.struct_lang_item(id).is_none()
            })
        }));

        for &target in Target::all() {
            let options = CompileOptions {
                target,
                ..Default::default()
            };
            let mut session = crate::CompilerSession::new();
            crate::test_support::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let semantic = session.rooted_cfg(&options).unwrap();
            session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap_or_else(|error| {
                    panic!("canonical source StrBuf functions should lower for {target}: {error}")
                });
        }
    }

    #[test]
    fn trusted_std_presence_does_not_create_a_bare_strbuf_prelude_name() {
        let context = ImportDiscoveryContext::new(1, "/project", Some("/sdk"), "test")
            .expect("discovery context should be valid");
        let mut assembler = DiscoverySourceAssembler::new(
            context,
            "/project/main.rue",
            "/project/main.rue",
            PhysicalFileIdentity::new(1, 1),
            FileMetadataFingerprint::new(1, 1, 1),
            Arc::new(
                r#"const std = @import("std/_std.rue");
                   fn main() -> i32 { let value: StrBuf = "x"; @intCast(value.len()) }"#
                    .to_owned(),
            ),
        )
        .unwrap();
        assembler
            .add_explicit(
                "/sdk/_std.rue",
                "/sdk/_std.rue",
                PhysicalFileIdentity::new(2, 2),
                FileMetadataFingerprint::new(2, 2, 2),
                Arc::new("pub const strbuf = @import(\"strbuf.rue\");".to_owned()),
            )
            .unwrap();
        assembler
            .add_explicit(
                "/sdk/strbuf.rue",
                "/sdk/strbuf.rue",
                PhysicalFileIdentity::new(3, 3),
                FileMetadataFingerprint::new(3, 3, 3),
                Arc::new(
                    "pub struct StrBuf { buf: ptr mut u8, len: u64, cap: u64, fn len(borrow self) -> u64 { self.len } }"
                        .to_owned(),
                ),
            )
            .unwrap();

        let snapshot = assembler.snapshot().unwrap();
        let errors = test_frontend_snapshot(&snapshot, &CompileOptions::default())
            .expect_err("importing std must not place StrBuf in the root module's bare namespace");
        assert!(
            errors.iter().any(
                |error| matches!(&error.kind, ErrorKind::UnknownType(name) if name == "StrBuf")
            ),
            "expected bare StrBuf to remain unknown: {errors:#?}"
        );
    }

    /// Trust comes from the captured standard-library root, never from how an
    /// import is spelled. A project module reached through an `std/`-shaped
    /// specifier resolves to an ordinary project module, so its `StrBuf` is a
    /// plain struct rather than the `StrBuf` language item.
    #[test]
    fn caller_std_shaped_specifier_cannot_spoof_strbuf_language_item() {
        let root = FileId::new(1);
        let spoof_file = FileId::new(2);
        let metadata = SourceMetadata::new(
            root,
            [
                (root, "/checkout/main.rue".to_owned()),
                (spoof_file, "/checkout/std/strbuf.rue".to_owned()),
            ]
            .into(),
            [
                (root, "main.rue".to_owned()),
                (spoof_file, "std/strbuf.rue".to_owned()),
            ]
            .into(),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(
            &[
                SourceView::new(
                    "/checkout/main.rue",
                    "const spoof = @import(\"std/strbuf.rue\"); fn main() -> i32 { let value = spoof.StrBuf { value: 7 }; value.value }",
                    root,
                ),
                SourceView::new(
                    "/checkout/std/strbuf.rue",
                    "pub struct StrBuf { value: i32 }",
                    spoof_file,
                ),
            ],
            metadata,
        )
        .unwrap();
        let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
            .expect("a caller-authored std-shaped specifier remains an ordinary module");
        assert!(semantic.type_pools().any(|pool| {
            pool.all_struct_ids().any(|id| {
                &*pool.struct_def(id).name == "StrBuf"
                    && pool.struct_lang_item(id).is_none()
                    && !pool.is_strbuf(id)
            })
        }));
    }

    #[test]
    fn documented_snapshot_session_embedding_carries_complete_identity() {
        let root = FileId::new(7);
        let paths = AHashMap::from([(root, "src/main.rue".to_owned())]);
        let metadata = SourceMetadata::new(root, paths.clone(), paths).unwrap();
        let snapshot = SourceSnapshot::new(
            metadata,
            vec![(root, Arc::new("fn main() -> i32 { 0 }".to_owned()))],
        )
        .unwrap();
        let options = CompileOptions::default();

        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let rir = session.canonical_rir().unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();

        assert_eq!(semantic.functions().len(), 1);
        assert_eq!(
            rir.source_revision(),
            snapshot.source_revision(),
            "the canonical RIR and rooted CFG request share the published source identity"
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
        let metadata = SourceMetadata::from_sources(&sources, root, AHashMap::new()).unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata.clone()).unwrap();

        let mut session = CompilerSession::new();
        let errors = session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap_err();
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
            AHashMap::from([(file_id, "expected.rue".to_string())]),
            AHashMap::from([(file_id, "stable.rue".to_string())]),
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
            AHashMap::from([
                (root, "main.rue".to_string()),
                (helper, "helper.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata.clone()).unwrap();

        let borrowed =
            test_compile_sources_with_metadata(&sources, &metadata, &CompileOptions::default())
                .unwrap();
        let owned = test_compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();

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
    fn publication_seams_leave_no_lease_reacquisition_cascades() {
        // RUE-1576 gate: the collection-scope handoffs replace per-node
        // lease-reacquisition demand cascades with borrowed cone authority.
        // Every remaining way a handoff can fail to cover a cone is
        // structural and program-independent, so one full one-worker compile
        // pinning both counters to zero is a complete regression detector:
        // a nonzero value here means a seam silently degraded and the
        // cascades returned. One worker is the exactness regime the scaling
        // contract already pins for deterministic work; parallel workers
        // carry a known residual seam in scope inheritance that a separate
        // issue tracks, so the gate deliberately matches the structural
        // probes rather than the worker matrix.
        let root = FileId::new(1);
        let helper = FileId::new(2);
        let sources = [
            SourceView::new(
                "/checkout/main.rue",
                "const helper = @import(\"helper.rue\");\n\
                 struct Pair { left: i32, right: i32 }\n\
                 fn main() -> i32 {\n\
                     let pair = Pair { left: helper.answer(), right: 2 };\n\
                     pair.left + pair.right\n\
                 }",
                root,
            ),
            SourceView::new(
                "/checkout/helper.rue",
                "pub fn answer() -> i32 { 40 }",
                helper,
            ),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            root,
            AHashMap::from([
                (root, "main.rue".to_string()),
                (helper, "helper.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
        let mut session = CompilerSession::with_query_concurrency(1);
        let discovery = crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut session)
            .unwrap();
        let output = crate::queries::compile_with_session(
            &mut session,
            &discovery.snapshot,
            &CompileOptions::default(),
        )
        .unwrap();
        let metrics = output.unstable_metrics();
        assert_eq!(
            metrics.publication.cone_retention_failures, 0,
            "every publication seam must hand its retained cone forward"
        );
        assert_eq!(
            metrics.query_runtime.validation.proof_reacquisition_misses, 0,
            "no scope may re-lease a cone its predecessor collection certified"
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
            AHashMap::from([
                (root, "main.rue".to_string()),
                (helper, "helper.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();

        // An import-bearing program is parsed by its discovery epoch, and the
        // batch pass that follows reuses that parse rather than repeating it.
        // So the one-pass claim splits in two: discovery lexes and parses each
        // module exactly once, and the batch adds no parse work at all.
        let mut session = CompilerSession::new();
        let discovery = crate::test_support::TestDiscoveryHost::new(&snapshot)
            .unwrap()
            .drive(&mut session)
            .unwrap();
        assert_eq!(discovery.parse_work.syntax.lexer_invocations, sources.len());
        assert_eq!(
            discovery.parse_work.syntax.parser_invocations,
            sources.len()
        );
        let output = crate::queries::compile_with_session(
            &mut session,
            &discovery.snapshot,
            &CompileOptions::default(),
        )
        .unwrap();
        let one_shot_metrics = output.unstable_metrics();
        let live_metrics = session.unstable_metrics();
        let live_runtime = live_metrics.query_runtime();
        assert_eq!(one_shot_metrics.query_runtime, live_runtime);
        assert_eq!(
            one_shot_metrics.semantic_reachability,
            live_metrics.semantic_reachability(),
        );
        assert!(
            one_shot_metrics.semantic_reachability.frontier_batches > 0
                && one_shot_metrics.semantic_reachability.frontier_keys >= 2,
            "a rooted cold compile reports the reachability frontiers it scheduled"
        );
        assert!(
            one_shot_metrics
                .query_runtime
                .validation
                .terminal_lease_observations
                > 0,
            "a rooted cold compile must publish its observed query terminals"
        );
        let stats = output.source_stats;
        let work = output.work;

        assert_eq!(stats.files, sources.len());
        assert_eq!(work.parsed.modules_reparsed, 0);
        assert_eq!(work.parsed.syntax.lexer_invocations, 0);
        assert_eq!(work.parsed.syntax.parser_invocations, 0);
        assert_eq!(work.merged.parser_invocations, 0);
        assert_eq!(work.merged.ast_payload_clones, 0);
        assert_eq!(work.merged.source_text_clones, 0);
        assert_eq!(work.merged.source_bytes_rehashed, 0);
        assert_eq!(work.lowered.parser_invocations, 0);
        assert_eq!(work.lowered.ast_payload_clones, 0);
        assert_eq!(work.lowered.source_text_clones, 0);
        assert_eq!(work.semantic.body_analysis.body_analyses_computed, 2);
        assert_eq!(work.semantic.body_analysis.body_analyses_reused, 0);
        assert_eq!(work.semantic.body_analysis.closure_bodies_visited, 2);
        assert_eq!(work.semantic.cfg.functions_considered, 2);
        assert_eq!(work.semantic.cfg.cfg_builds_attempted, 2);
        assert_eq!(work.semantic.cfg.cfg_builds_succeeded, 2);
        assert_eq!(work.semantic.cfg.optimization_attempts, 2);
        assert_eq!(work.semantic.cfg.optimization_completions, 2);

        // The query-native path does not execute the retired whole-program
        // declaration, binding, or durable-body phases. Their counters remain
        // zero while the rooted body and CFG work above reports what did run.
        let mut phase_work = work.semantic;
        phase_work.body_analysis = Default::default();
        phase_work.cfg = Default::default();
        assert_eq!(phase_work, CanonicalSemanticWork::default());
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
            // (RUE-920) A second top-level `main` in a non-root module is no
            // longer a frontend error, so it cannot appear in this
            // error-determinism corpus; a same-file duplicate still is one.
            (
                "fn main() -> i32 { 1 } fn main() -> i32 { 2 }",
                "fn helper() {}",
            ),
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
                let metadata =
                    SourceMetadata::from_sources(&sources, sources[0].file_id, AHashMap::new())
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
                AHashMap::from([
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
                let canonical = test_compile_snapshot(snapshot, &options).unwrap();
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

        let missing_linker = CompileOptions {
            linker: LinkerMode::System("/definitely/missing/rue-linker".to_string()),
            ..CompileOptions::default()
        };
        compile_snapshot(&snapshots[0], &missing_linker).unwrap_err();
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
    fn test_embedded_runtimes_are_valid() {
        for &target in Target::all() {
            validate_runtime(target)
                .unwrap_or_else(|error| panic!("embedded {target} runtime is invalid: {error}"));
        }
    }

    #[test]
    fn test_runtime_validation_rejects_archive_without_objects() {
        let err = parse_runtime_archive(b"!<arch>\n")
            .expect_err("empty archive wrapper must not validate as a usable runtime");

        assert_eq!(err, "embedded rue-runtime archive contains no object files");
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
    fn phase2_free_function_inlining_is_structural_and_request_local() {
        let snapshot = SourceSnapshot::single(
            "<phase2-inline-structure>",
            "fn add_one(x: i32) -> i32 { x + 1 } fn main() -> i32 { add_one(4) + add_one(5) }",
        )
        .unwrap();
        let call_count = |output: &RootedCfgOutput, name: &str| {
            output
                .cfgs
                .iter()
                .find(|unit| unit.record.codegen.defined_symbol.ends_with(name))
                .map(|unit| {
                    unit.record
                        .cfg
                        .blocks()
                        .iter()
                        .flat_map(|block| block.insts.iter())
                        .filter(|value| {
                            matches!(
                                unit.record.cfg.get_inst(**value).data,
                                rue_cfg::CfgInstData::Call { .. }
                            )
                        })
                        .count()
                })
                .unwrap_or_default()
        };
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();

        for opt_level in [rue_cfg::OptLevel::O0, rue_cfg::OptLevel::O1] {
            let options = CompileOptions {
                opt_level,
                ..CompileOptions::default()
            };
            let output = session.rooted_cfg(&options).unwrap();
            assert_eq!(call_count(&output, "main"), 2);
            assert!(
                output
                    .cfgs
                    .iter()
                    .all(|unit| unit.record.durable_reuse_allowed)
            );
        }

        for opt_level in [rue_cfg::OptLevel::O2, rue_cfg::OptLevel::O3] {
            let options = CompileOptions {
                opt_level,
                ..CompileOptions::default()
            };
            let output = session.rooted_cfg(&options).unwrap();
            assert_eq!(call_count(&output, "main"), 0);
            assert!(
                output
                    .cfgs
                    .iter()
                    .find(|unit| unit.record.codegen.defined_symbol.ends_with("main"))
                    .is_some_and(|unit| !unit.record.durable_reuse_allowed)
            );
            assert!(output.cfgs.iter().any(|unit| matches!(
                &unit.function,
                FunctionInstanceKey::Definition(definition) if definition.name() == "add_one"
            )));
            let first_generation = output.optimized_cfg_batch.generation;
            let second = session.rooted_cfg(&options).unwrap();
            assert_ne!(first_generation, second.optimized_cfg_batch.generation);
            assert_eq!(call_count(&second, "main"), 0);
            assert!(session
                .rooted_cfg_executions()
                .iter()
                .any(|(function, execution)| matches!(function, FunctionInstanceKey::Definition(definition) if definition.name() == "add_one")
                    && matches!(execution, rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined)));
        }
    }

    #[test]
    fn phase2_excludes_methods_from_general_inlining() {
        let snapshot = SourceSnapshot::single(
            "<phase2-method-exclusion>",
            "struct Counter { value: i32, fn next(self) -> i32 { self.value + 1 } } fn main() -> i32 { Counter { value: 4 }.next() }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let output = session
            .rooted_cfg(&CompileOptions {
                opt_level: rue_cfg::OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();
        let main = output
            .cfgs
            .iter()
            .find(|unit| unit.record.codegen.defined_symbol.ends_with("main"))
            .unwrap();
        assert!(main.record.durable_reuse_allowed);
        assert!(
            main.record
                .cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter())
                .any(|value| {
                    matches!(
                        main.record.cfg.get_inst(*value).data,
                        rue_cfg::CfgInstData::Call { .. }
                    )
                })
        );
    }

    #[test]
    fn phase2_single_call_token_destructor_is_actually_inlined() {
        let snapshot = SourceSnapshot::single(
            "<phase2-token-inline>",
            "struct Token { value: i32 } drop fn Token(self) { @dbg(self.value); } fn consume(token: Token) -> i32 { token.value + 1 } fn main() -> i32 { consume(Token { value: 7 }) }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let call_count = |output: &RootedCfgOutput| {
            let record = &output
                .cfgs
                .iter()
                .find(|unit| {
                    matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                })
                .unwrap()
                .record;
            record
                .cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter())
                .filter(|value| {
                    matches!(
                        record.cfg.get_inst(**value).data,
                        rue_cfg::CfgInstData::Call { .. }
                    )
                })
                .count()
        };
        let o0 = session
            .rooted_cfg(&CompileOptions {
                opt_level: rue_cfg::OptLevel::O0,
                ..CompileOptions::default()
            })
            .unwrap();
        assert_eq!(call_count(&o0), 1);
        assert!(o0.cfgs.iter().find(|unit| matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "main")).unwrap().record.durable_reuse_allowed);
        let o2 = session
            .rooted_cfg(&CompileOptions {
                opt_level: rue_cfg::OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();
        assert_eq!(call_count(&o2), 0);
        assert!(!o2.cfgs.iter().find(|unit| matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "main")).unwrap().record.durable_reuse_allowed);
        assert!(o2.cfgs.iter().any(|unit| matches!(
            &unit.function,
            FunctionInstanceKey::Definition(definition) if definition.name() == "consume"
        )));
    }

    #[test]
    fn phase2_codegen_consumes_the_exact_inlined_batch_on_both_backends() {
        let snapshot = SourceSnapshot::single(
            "<phase2-inline-codegen>",
            "fn add_one(x: i32) -> i32 { x + 1 } fn main() -> i32 { add_one(4) }",
        )
        .unwrap();

        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let options = CompileOptions {
                target,
                opt_level: rue_cfg::OptLevel::O2,
                ..CompileOptions::default()
            };
            let mut session = CompilerSession::new();
            session
                .update_for_presentation(&snapshot)
                .into_result()
                .unwrap();

            let first = session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let main_cfg = first
                .cfgs
                .iter()
                .find(|unit| matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "main"))
                .unwrap();
            assert!(!main_cfg.record.durable_reuse_allowed);
            assert!(
                main_cfg
                    .record
                    .cfg
                    .blocks()
                    .iter()
                    .flat_map(|block| block.insts.iter())
                    .all(|value| !matches!(
                        main_cfg.record.cfg.get_inst(*value).data,
                        rue_cfg::CfgInstData::Call { .. }
                    ))
            );
            let main_unit = first
                .units
                .iter()
                .find(|unit| matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "main"))
                .unwrap();
            let callee_unit = first
                .units
                .iter()
                .find(|unit| matches!(&unit.function, FunctionInstanceKey::Definition(definition) if definition.name() == "add_one"))
                .unwrap();
            assert!(
                main_unit
                    .unit
                    .relocations
                    .iter()
                    .all(|relocation| relocation.symbol != callee_unit.unit.defined_symbol),
                "the {target} caller must not retain a relocation to its inlined callee: {:?}",
                main_unit.unit.relocations
            );

            session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let executions = session.codegen_executions();
            assert!(executions.iter().any(|(function, execution)| {
                matches!(function, FunctionInstanceKey::Definition(definition) if definition.name() == "main")
                    && *execution == rue_query::RequestExecution::Computed
            }), "{target}: {executions:?}");
            assert!(executions.iter().any(|(function, execution)| {
                matches!(function, FunctionInstanceKey::Definition(definition) if definition.name() == "add_one")
                    && matches!(execution, rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined)
            }), "{target}: {executions:?}");
        }
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
    fn generated_loop_and_place_locals_are_hygienic_against_source_names() {
        let output = test_compile_source(
            "fn index() -> u64 { 0 } \
             fn main() -> i32 { \
                 let __rue_for_p_0 = 40; \
                 let __rue_place_0 = 2; \
                 for _ in [1, 2] {} \
                 let mut values = [0, 0, 0, 0]; \
                 values[index()] += 1; \
                 __rue_for_p_0 + __rue_place_0 \
             }",
        )
        .expect("source-legal names cannot alias source-impossible compiler locals");
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn wildcard_for_binder_keeps_unused_warning_suppression() {
        let output = test_compile_source("fn main() -> i32 { for _ in [1, 2] {} 0 }").unwrap();
        assert!(output.warnings.is_empty());
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
    fn test_cross_file_main_is_root_module_scoped() {
        // RUE-920: `main` is scoped to the root module, not program-wide unique.
        // With a.rue as root (first source), b.rue's `main` is an ordinary
        // namespaced function, so the program compiles instead of failing with
        // a duplicate-main error. The root's `main` remains the entry point.
        let sources = vec![
            SourceView::new("a.rue", "fn main() -> i32 { 1 }", FileId::new(1)),
            SourceView::new("b.rue", "fn main() -> i32 { 2 }", FileId::new(2)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-module `main` must compile under RUE-920: {:?}",
            result.err()
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
                AHashMap::from([
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
                .map(|func| func.legacy_name().to_owned())
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
                    r#"struct Owned { value: i32 }
                    drop fn Owned(self) {}
                    pub struct Payload {
                        value: i32,
                        text: Owned,
                        fn score(borrow self) -> i32 { self.value }
                    }
                    drop fn Payload(self) { @dbg(self.value); }
                    pub enum Choice { Empty, Text(Owned) }
                    pub fn entry() -> i32 {
                        let payload = Payload { value: 10, text: Owned { value: 1 } };
                        let choice = Choice.Empty;
                        match choice { Choice.Empty => payload.score(), Choice.Text(value) => value.value }
                    }"#,
                    left_id,
                ),
                SourceView::new(
                    &right_path,
                    r#"struct Owned { value: i32 }
                    drop fn Owned(self) {}
                    pub struct Payload {
                        value: i32,
                        text: Owned,
                        fn score(borrow self) -> i32 { self.value }
                    }
                    drop fn Payload(self) { @dbg(self.value); }
                    pub enum Choice { Empty, Text(Owned) }
                    pub fn entry() -> i32 {
                        let payload = Payload { value: 20, text: Owned { value: 2 } };
                        let choice = Choice.Empty;
                        match choice { Choice.Empty => payload.score(), Choice.Text(value) => value.value }
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
                AHashMap::from([
                    (FileId::new(1), "main.rue".to_string()),
                    (left_id, "left/shared.rue".to_string()),
                    (right_id, "right/shared.rue".to_string()),
                ]),
            )
            .unwrap();
            let snapshot = SourceSnapshot::from_sources(&sources, source_metadata).unwrap();
            let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
                .expect("frontend should compile");
            let mut names = std::collections::BTreeSet::new();
            for pool in semantic.type_pools() {
                for id in pool.all_struct_ids() {
                    if &*pool.struct_def(id).name == "Payload" {
                        names.insert(format!("struct:{}", pool.struct_symbol_name(id)));
                    }
                }
                for id in pool.all_enum_ids() {
                    if &*pool.enum_def(id).name == "Choice" {
                        names.insert(format!("enum:{}", pool.enum_symbol_name(id)));
                    }
                }
            }
            for function in semantic.functions() {
                let name = &function.record.source_name;
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
            SourceView::new(
                "main.rue",
                "const left = @import(\"left/clash.rue\"); const right = @import(\"right/clash.rue\"); fn main() -> i32 { left.consume() + right.consume() }",
                main_id,
            ),
            SourceView::new(
                "left/clash.rue",
                "struct OwnedLeft {} drop fn OwnedLeft(self) {} pub struct Clash { text: OwnedLeft } pub fn consume() -> i32 { let value = Clash { text: OwnedLeft {} }; 0 }",
                struct_id,
            ),
            SourceView::new(
                "right/clash.rue",
                "struct OwnedRight {} drop fn OwnedRight(self) {} pub enum Clash { Text(OwnedRight) } pub fn consume() -> i32 { let value = Clash.Text(OwnedRight {}); 0 }",
                enum_id,
            ),
        ];

        let source_metadata = SourceMetadata::from_sources(
            &sources,
            main_id,
            AHashMap::from([
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
            .map(|function| function.record.source_name.as_ref())
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
    fn zst_drop_glue_lowers_on_every_backend_with_canonical_aggregate_widths() {
        let state = test_cfg(
            r#"
            struct D { value: i32 }
            drop fn D(self) { @dbg(self.value); }
            struct Inner { leading: (), item: D, interior: (), text: D }
            enum E { Full((), Inner, (), D), Empty }
            fn main() -> i32 {
                let values = [
                    E.Full((), Inner { leading: (), item: D { value: 1 }, interior: (), text: D { value: 3 } }, (), D { value: 5 }),
                    E.Full((), Inner { leading: (), item: D { value: 2 }, interior: (), text: D { value: 4 } }, (), D { value: 6 }),
                ];
                0
            }
            "#,
        )
        .expect("ZST-interleaved drop aggregates should reach CFG lowering");

        let drop_glue = state
            .functions
            .iter()
            .filter(|function| function.record.source_name.starts_with("__rue_drop_"))
            .collect::<Vec<_>>();
        assert!(
            drop_glue.len() >= 3,
            "struct, enum, and array glue are rooted"
        );
        for function in drop_glue {
            assert_eq!(
                function.record.num_param_slots,
                function.record.cfg.num_params(),
                "{}",
                function.record.source_name
            );
        }

        for &target in Target::all() {
            test_codegen_state(&state, target)
                .unwrap_or_else(|error| panic!("drop glue should lower for {target}: {error}"));
        }
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
        let metadata =
            SourceMetadata::from_sources(&sources, FileId::new(1), AHashMap::new()).unwrap();
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
