//! In-process canonical parse, merge, and RIR query orchestration.

use std::sync::Arc;

use crate::{
    CanonicalMergeWork, CanonicalMergedProgram, CanonicalParseSession, CanonicalRirOutput,
    CanonicalRirWork, CanonicalSemanticOutput, CanonicalSemanticWork, CodegenInputDescriptor,
    CompileError, CompileErrors, CompileOptions, ErrorKind, ParseInvalidationSummary,
    ParsedModulesWork, SemanticInputDescriptor, SourceSnapshot, analyze_canonical_program,
    lower_canonical_rir, merge_parsed_modules, parsed_modules::ParsedProgram,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendQueryWork {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalFrontendSessionWork {
    pub updates: usize,
    pub last_parse: ParsedModulesWork,
    pub last_invalidation: ParseInvalidationSummary,
    pub merge: FrontendQueryWork,
    pub rir: FrontendQueryWork,
    pub downstream_invalidations: usize,
    pub last_merge: CanonicalMergeWork,
    pub last_rir: CanonicalRirWork,
    pub semantic: FrontendQueryWork,
    pub semantic_entries: usize,
    pub semantic_entries_invalidated: usize,
    pub semantic_records: Vec<SemanticQueryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQueryRecord {
    pub input: CodegenInputDescriptor,
    pub work: CanonicalSemanticWork,
    pub failed: bool,
}

#[derive(Debug)]
pub struct CanonicalFrontendUpdate {
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    work: ParsedModulesWork,
    invalidation: ParseInvalidationSummary,
    downstream_invalidated: bool,
}

impl CanonicalFrontendUpdate {
    pub fn result(&self) -> Result<&Arc<ParsedProgram>, &CompileErrors> {
        self.result.as_ref()
    }
    pub fn into_result(self) -> Result<Arc<ParsedProgram>, CompileErrors> {
        self.result
    }
    pub fn work(&self) -> ParsedModulesWork {
        self.work
    }
    pub fn invalidation(&self) -> &ParseInvalidationSummary {
        &self.invalidation
    }
    pub fn downstream_invalidated(&self) -> bool {
        self.downstream_invalidated
    }
}

#[derive(Debug, Default)]
pub struct CanonicalFrontendSession {
    parse: CanonicalParseSession,
    published: Option<Arc<ParsedProgram>>,
    merge_cache: Option<Result<Arc<CanonicalMergedProgram>, CompileErrors>>,
    rir_cache: Option<Arc<CanonicalRirOutput>>,
    semantic_cache: Vec<SemanticCacheEntry>,
    work: CanonicalFrontendSessionWork,
}

#[derive(Debug)]
struct SemanticCacheEntry {
    input: CodegenInputDescriptor,
    result: Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
}

impl CanonicalFrontendSession {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn published(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
    }
    pub fn work(&self) -> &CanonicalFrontendSessionWork {
        &self.work
    }

    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CanonicalFrontendUpdate {
        self.work.updates += 1;
        let update = self.parse.update(snapshot);
        let parse_work = update.work();
        let invalidation = update.invalidation().clone();
        self.work.last_parse = parse_work;
        self.work.last_invalidation = invalidation.clone();
        match update.into_result() {
            Ok(candidate) => {
                let exact = self.published.as_deref().is_some_and(|published| {
                    programs_are_pointer_equivalent(published, &candidate)
                });
                let downstream_invalidated = self.published.is_some() && !exact;
                if exact {
                    CanonicalFrontendUpdate {
                        result: Ok(self.published.as_ref().unwrap().clone()),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated: false,
                    }
                } else {
                    if downstream_invalidated {
                        self.work.downstream_invalidations += 1;
                    }
                    self.merge_cache = None;
                    self.rir_cache = None;
                    self.work.semantic_entries_invalidated += self.semantic_cache.len();
                    self.semantic_cache.clear();
                    self.work.last_merge = CanonicalMergeWork::default();
                    self.work.last_rir = CanonicalRirWork::default();
                    self.work.semantic_entries = 0;
                    self.work.semantic_records.clear();
                    self.published = Some(candidate.clone());
                    CanonicalFrontendUpdate {
                        result: Ok(candidate),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated,
                    }
                }
            }
            Err(errors) => CanonicalFrontendUpdate {
                result: Err(errors),
                work: parse_work,
                invalidation,
                downstream_invalidated: false,
            },
        }
    }

    pub fn merge(&mut self) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.work.merge.calls += 1;
        if let Some(cached) = &self.merge_cache {
            self.work.merge.reuses += 1;
            return cached.clone();
        }
        let parsed = self.published.as_deref().ok_or_else(no_published_program)?;
        self.work.merge.executions += 1;
        let merged = merge_parsed_modules(parsed).map(Arc::new);
        if let Ok(merged) = &merged {
            debug_assert_eq!(merged.ast().source_revision(), parsed.source_revision());
            self.work.last_merge = merged.work();
        }
        self.merge_cache = Some(merged.clone());
        merged
    }

    pub fn rir(&mut self) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.work.rir.calls += 1;
        if let Some(cached) = &self.rir_cache {
            self.work.rir.reuses += 1;
            return Ok(cached.clone());
        }
        let merged = self.merge()?;
        self.work.rir.executions += 1;
        let rir = Arc::new(lower_canonical_rir(&merged).map_err(CompileErrors::from)?);
        debug_assert_eq!(rir.source_revision(), merged.ast().source_revision());
        self.work.last_rir = rir.work();
        self.rir_cache = Some(rir.clone());
        Ok(rir)
    }

    /// Analyze the current published revision without issuing stable definition IDs.
    pub fn semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
        self.work.semantic.calls += 1;
        let rir = self.rir()?;
        let merged = match self.merge_cache.as_ref() {
            Some(Ok(merged)) => merged.clone(),
            Some(Err(errors)) => return Err(errors.clone()),
            None => unreachable!("successful RIR query retains its merge input"),
        };
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                merged.definitions().source_snapshot(),
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        if let Some(entry) = self
            .semantic_cache
            .iter()
            .find(|entry| entry.input == input)
        {
            self.work.semantic.reuses += 1;
            return entry.result.clone();
        }

        self.work.semantic.executions += 1;
        let result = analyze_canonical_program(&merged, &rir, options, false).map(Arc::new);
        let semantic_work = result
            .as_ref()
            .map(|output| output.work())
            .unwrap_or_default();
        if let Ok(output) = &result {
            debug_assert_eq!(output.input(), &input);
            debug_assert_eq!(semantic_work.binding.bind_invocations, 1);
            debug_assert_eq!(semantic_work.manifest.build_invocations, 0);
            debug_assert!(!semantic_work.stable_ids_requested);
        }
        self.semantic_cache.push(SemanticCacheEntry {
            input: input.clone(),
            result: result.clone(),
        });
        self.work.semantic_entries = self.semantic_cache.len();
        self.work.semantic_records.push(SemanticQueryRecord {
            input,
            work: semantic_work,
            failed: result.is_err(),
        });
        result
    }
}

fn programs_are_pointer_equivalent(left: &ParsedProgram, right: &ParsedProgram) -> bool {
    left.source_revision() == right.source_revision()
        && left.modules().len() == right.modules().len()
        && left
            .modules()
            .iter()
            .zip(right.modules())
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

fn no_published_program() -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        "frontend query session has no successful parsed program".to_string(),
    )))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use rue_span::FileId;

    use super::*;
    use crate::{
        LinkerMode, OptLevel, PreviewFeature, PreviewFeatures, SourceMetadata, SourceSnapshot,
        Target,
    };

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn base() -> SourceSnapshot {
        snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        )
    }

    #[test]
    fn repeated_queries_and_noop_update_retain_pointer_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalFrontendSession>();
        assert_send_sync::<CanonicalMergedProgram>();
        assert_send_sync::<CanonicalRirOutput>();

        let source = base();
        let mut session = CanonicalFrontendSession::new();
        let first_program = session.update(&source).into_result().unwrap();
        let first_merge = session.merge().unwrap();
        let second_merge = session.merge().unwrap();
        let first_rir = session.rir().unwrap();
        let second_rir = session.rir().unwrap();
        assert!(Arc::ptr_eq(&first_merge, &second_merge));
        assert!(Arc::ptr_eq(&first_rir, &second_rir));

        let noop = session.update(&source);
        assert!(!noop.downstream_invalidated());
        let second_program = noop.into_result().unwrap();
        assert!(Arc::ptr_eq(&first_program, &second_program));
        assert!(Arc::ptr_eq(&first_merge, &session.merge().unwrap()));
        assert!(Arc::ptr_eq(&first_rir, &session.rir().unwrap()));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().downstream_invalidations, 0);

        let published = session.published().unwrap().clone();
        let merged = first_merge.clone();
        let rir = first_rir.clone();
        std::thread::spawn(move || {
            assert_eq!(published.modules().len(), 2);
            assert_eq!(merged.ast().modules().len(), 2);
            assert!(!rir.rir().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn one_edit_among_128_recomputes_downstream_once() {
        let make = |edited: bool| {
            let physical = (0..128)
                .map(|index| (FileId::new(index), format!("/p/m{index}.rue")))
                .collect();
            let logical = (0..128)
                .map(|index| (FileId::new(index), format!("m{index}.rue")))
                .collect();
            let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                (0..128)
                    .map(|index| {
                        let value = if edited && index == 81 { 2 } else { 1 };
                        (
                            FileId::new(index),
                            Arc::new(format!("fn f{index}() -> i32 {{ {value} }}")),
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut session = CanonicalFrontendSession::new();
        session.update(&make(false)).into_result().unwrap();
        session.rir().unwrap();
        let update = session.update(&make(true));
        assert!(update.downstream_invalidated());
        assert_eq!(update.work().modules_reused, 127);
        assert_eq!(update.work().modules_reparsed, 1);
        session.rir().unwrap();
        session.rir().unwrap();
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 2);
        assert_eq!(session.work().downstream_invalidations, 1);
    }

    #[test]
    fn syntax_failure_preserves_published_revision_and_cached_queries() {
        let source = base();
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CanonicalFrontendSession::new();
        let program = session.update(&source).into_result().unwrap();
        let merged = session.merge().unwrap();
        let rir = session.rir().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result().is_err());
        assert!(!failed.downstream_invalidated());
        assert!(Arc::ptr_eq(session.published().unwrap(), &program));
        assert!(Arc::ptr_eq(&session.merge().unwrap(), &merged));
        assert!(Arc::ptr_eq(&session.rir().unwrap(), &rir));
    }

    #[test]
    fn duplicate_merge_error_is_memoized_and_recovery_invalidates_it() {
        let duplicate = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn same() {} fn same() {} fn main() {}",
            )],
            1,
        );
        let fixed = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&duplicate).into_result().unwrap();
        let first = session.merge().unwrap_err();
        let second = session.merge().unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert!(session.rir().is_err());
        assert!(session.semantic(&CompileOptions::default()).is_err());
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);

        let update = session.update(&fixed);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        assert!(session.rir().is_ok());
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 1);
    }

    #[test]
    fn root_relocation_file_id_and_logical_changes_invalidate_correctly() {
        let base = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let root_only = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let relocated = snapshot(
            &[
                (1, "/new/a.rue", "a.rue", "fn a() {}"),
                (2, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let reassigned = snapshot(
            &[
                (11, "/new/a.rue", "a.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&base).into_result().unwrap();
        session.rir().unwrap();

        let root = session.update(&root_only);
        assert!(root.downstream_invalidated());
        assert_eq!(root.work().modules_reused, 2);
        root.into_result().unwrap();
        session.rir().unwrap();
        let moved = session.update(&relocated);
        assert!(moved.downstream_invalidated());
        assert_eq!(moved.work().modules_rebound, 2);
        moved.into_result().unwrap();
        session.rir().unwrap();
        let ids = session.update(&reassigned);
        assert!(ids.downstream_invalidated());
        assert_eq!(ids.work().modules_reparsed, 2);
        ids.into_result().unwrap();
        session.rir().unwrap();
        let rename = session.update(&renamed);
        assert!(rename.downstream_invalidated());
        assert_eq!(rename.invalidation().added.len(), 1);
        assert_eq!(rename.invalidation().removed.len(), 1);
        assert_eq!(rename.work().modules_rebound, 1);
    }

    #[test]
    fn semantic_queries_reuse_by_codegen_identity_and_ignore_linker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalSemanticOutput>();

        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        let first = session.semantic(&options).unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let linker_only = CompileOptions {
            linker: LinkerMode::System("unused-linker".to_string()),
            ..options.clone()
        };
        assert!(Arc::ptr_eq(
            &first,
            &session.semantic(&linker_only).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(first.work().binding.bind_invocations, 1);
        assert_eq!(first.work().manifest.build_invocations, 0);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.functions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn semantic_option_variants_create_deterministic_distinct_entries() {
        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let default = CompileOptions::default();
        session.semantic(&default).unwrap();
        session
            .semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..default.clone()
            })
            .unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .semantic(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        let work = session.work();
        assert_eq!(work.semantic.executions, 4);
        assert_eq!(work.semantic_entries, 4);
        assert_eq!(work.semantic_records.len(), 4);
        assert!(work.semantic_records.iter().all(|record| {
            !record.failed
                && record.work.binding.bind_invocations == 1
                && record.work.manifest.build_invocations == 0
        }));
        for (index, left) in work.semantic_records.iter().enumerate() {
            assert!(
                work.semantic_records[index + 1..]
                    .iter()
                    .all(|right| left.input != right.input)
            );
        }
    }

    #[test]
    fn semantic_cache_invalidates_on_edit_but_survives_failed_parse() {
        let source = base();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let first = session.semantic(&options).unwrap();
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(&first, &session.semantic(&options).unwrap()));
        let update = session.update(&edited);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn semantic_errors_are_memoized_and_recovery_reexecutes() {
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            1,
        );
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&invalid).into_result().unwrap();
        let first = session.semantic(&options).unwrap_err();
        let second = session.semantic(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.semantic(&options).is_ok());
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }
}
