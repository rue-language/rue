use std::path::Path;

use rue_compiler::unstable::TestCandidateInventory;
use rue_compiler::unstable::{
    CancellableCompileOutcome, CodegenReady, CompilationCancellation, ObjectsReady,
    PresentationOutput, PresentationRequest, cancellable_executable_in_compile_scope,
    codegen_ready, executable_in_compile_scope, objects_ready, runnable_ready,
};
use rue_compiler::{
    AcceptedReadManifest, CompileErrors, CompileOptions, CompileOutput, ImportDiscoveryContext,
    ImportDiscoveryStatus, ImportDiscoveryView, MultiErrorResult, RirView, SourceSnapshot,
};

use crate::source_loader::{
    ImportDiscoveryResult, SourceLoadError, SourceLoadRequest, WatchInput,
    acquire_reached_toolchain_modules, acquire_reached_toolchain_modules_superseding, load,
    reload_from_filesystem,
};

/// Immutable filesystem configuration captured when a retained host opens.
pub struct HostOpenRequest<'a> {
    pub root_source: &'a str,
    pub source_manifest_path: Option<&'a str>,
    pub std_root: Option<&'a Path>,
}

/// One canonical filesystem observer and retained compiler session.
pub struct FilesystemCompilerHost {
    state: ImportDiscoveryResult,
}

impl FilesystemCompilerHost {
    /// Open a root source and drive parser-owned import discovery to closure.
    pub fn open(request: HostOpenRequest<'_>) -> Result<Self, SourceLoadError> {
        load(SourceLoadRequest {
            root_source: request.root_source,
            source_manifest_path: request.source_manifest_path,
            std_root: request.std_root,
        })
        .map(|state| Self { state })
    }

    /// Re-observe the exact accepted-read closure and publish its successor.
    pub fn reobserve(&mut self) -> Result<(), SourceLoadError> {
        reload_from_filesystem(&mut self.state, None)
    }

    /// Re-observe like [`Self::reobserve`], aborting promptly with
    /// [`SourceLoadError::Superseded`] once `supersession` reports a newer
    /// source revision (RUE-1830). A superseded attempt never exposes partial
    /// state: a pre-commit abort keeps the prior snapshot, manifest, and graph,
    /// while a signal observed after close may retain that one coherent closed
    /// successor. The caller restarts from the newest bytes either way.
    pub fn reobserve_superseding(
        &mut self,
        supersession: &dyn Fn() -> bool,
    ) -> Result<(), SourceLoadError> {
        reload_from_filesystem(&mut self.state, Some(supersession))
    }

    /// Satisfy compiler-issued reached-body toolchain demands for this revision.
    pub fn acquire_reached_toolchain_modules(
        &mut self,
        options: &CompileOptions,
    ) -> Result<(), SourceLoadError> {
        acquire_reached_toolchain_modules(&mut self.state, options)
    }

    /// Acquire like [`Self::acquire_reached_toolchain_modules`], aborting
    /// promptly with [`SourceLoadError::Superseded`] once `supersession`
    /// reports a newer source revision (RUE-1863). A superseded acquisition
    /// never exposes partial state: a pre-commit abort keeps the prior state,
    /// while a signal observed after publication may retain that one coherent
    /// committed acquisition round.
    pub fn acquire_reached_toolchain_modules_superseding(
        &mut self,
        options: &CompileOptions,
        supersession: &dyn Fn() -> bool,
    ) -> Result<(), SourceLoadError> {
        acquire_reached_toolchain_modules_superseding(&mut self.state, options, Some(supersession))
    }

    pub fn source_snapshot(&self) -> &SourceSnapshot {
        &self.state.source_snapshot
    }

    pub fn discovery_revision(&self) -> &ImportDiscoveryView {
        &self.state.revision
    }

    pub fn discovery_status(&self) -> ImportDiscoveryStatus {
        self.state.revision.status()
    }

    pub fn accepted_reads(&self) -> &AcceptedReadManifest {
        &self.state.read_manifest
    }

    /// Exact accepted filesystem closure, plus the policy manifest that can
    /// change which reads are allowed on the next observation.
    pub fn watch_inputs(&self) -> Vec<WatchInput> {
        self.state.watch_inputs()
    }

    pub fn root_path(&self) -> &Path {
        &self.state.resolution.root_path
    }

    pub fn discovery_context(&self) -> &ImportDiscoveryContext {
        &self.state.resolution.context
    }

    /// Acquire the declared test-candidate inventory (ADR-0083 §1) under this
    /// host's read policy.
    ///
    /// `declared` is the build system's `srcs` list as `--test-candidates`
    /// spelled it: project-root-relative paths. Each is resolved against the
    /// project root and observed the way an import candidate is observed —
    /// with a `--source-manifest`, an undeclared spelling or a disallowed
    /// canonical file is `Unreadable` rather than a filesystem read; a missing
    /// file is `Absent`; an I/O or UTF-8 failure is `Unreadable` with its
    /// reason. The inventory is built from this host's own discovery context,
    /// so it can never be reported against a closure acquired under another
    /// read regime.
    ///
    /// Candidate reads are deliberately not added to the accepted-read
    /// manifest: they are not inputs of the program being built and must not
    /// appear in `--emit deps` or the watch closure. The compiler publishes them
    /// as their own revisioned input leaves when a request reports against the
    /// inventory (`rue_compiler::unstable::unimported_test_files`).
    pub fn acquire_test_candidates(
        &self,
        declared: &[String],
    ) -> Result<TestCandidateInventory, CompileErrors> {
        let context = self.discovery_context();
        let project_root = Path::new(context.project_root());
        let mut inventory = TestCandidateInventory::new(context);
        for path in declared {
            let outcome = self.state.read_test_candidate(&project_root.join(path));
            inventory
                .declare(path, outcome)
                .map_err(CompileErrors::from)?;
        }
        Ok(inventory)
    }

    /// Return owned unstable instrumentation without exposing the retained
    /// session or any query artifacts.
    pub fn unstable_metrics(&self) -> rue_compiler::unstable::MetricsSnapshot {
        self.state.session.unstable_metrics()
    }

    /// Query RIR through the retained session for the CLI presentation path.
    pub fn rir(&mut self) -> MultiErrorResult<std::sync::Arc<RirView>> {
        self.state.session.rir()
    }

    /// Produce one unstable presentation without exposing the session owner.
    pub fn present(
        &mut self,
        request: PresentationRequest<'_>,
    ) -> MultiErrorResult<PresentationOutput> {
        self.state.session.unstable_present(request)
    }

    /// Preserve the command-line compiler's existing one-shot timed adapter.
    pub fn executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        executable_in_compile_scope(&mut self.state.session, options)
    }

    pub fn cancellable_executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> CancellableCompileOutcome {
        cancellable_executable_in_compile_scope(&mut self.state.session, options, cancellation)
    }

    /// Reach ADR-0068's codegen-ready endpoint without projecting objects.
    pub fn codegen_ready(&mut self, options: &CompileOptions) -> MultiErrorResult<CodegenReady> {
        codegen_ready(&mut self.state.session, options)
    }

    /// Continue a compiler-issued codegen-ready capability to objects-ready.
    pub fn objects_ready(&mut self, ready: CodegenReady) -> MultiErrorResult<ObjectsReady> {
        objects_ready(&mut self.state.session, ready)
    }

    /// Continue a compiler-issued objects-ready capability through fresh link.
    pub fn runnable_ready(&mut self, ready: ObjectsReady) -> MultiErrorResult<CompileOutput> {
        runnable_ready(&mut self.state.session, ready)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rue-retained-host-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str, source: &str) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, source).unwrap();
            path
        }

        fn open(&self) -> FilesystemCompilerHost {
            let root = self.path.join("main.rue");
            FilesystemCompilerHost::open(HostOpenRequest {
                root_source: root.to_str().unwrap(),
                source_manifest_path: None,
                std_root: None,
            })
            .unwrap()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run_to_runnable(host: &mut FilesystemCompilerHost) -> CompileOutput {
        let options = CompileOptions::default();
        let codegen = host.codegen_ready(&options).unwrap();
        let objects = host.objects_ready(codegen).unwrap();
        host.runnable_ready(objects).unwrap()
    }

    #[test]
    fn no_op_reobservation_reuses_the_retained_frontend() {
        let dir = TestDir::new("noop");
        dir.write(
            "main.rue",
            "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        dir.write("helper.rue", "pub fn value() -> i32 { 7 }\n");
        let mut host = dir.open();

        let first = run_to_runnable(&mut host);
        let before = host.state.session.unstable_metrics();
        host.reobserve().unwrap();
        let second = run_to_runnable(&mut host);
        let after = host.state.session.unstable_metrics();

        assert_eq!(first.elf, second.elf);
        assert!(after.updates() > before.updates());
        assert_eq!(after.rir().executions, before.rir().executions);
    }

    #[test]
    fn body_edit_matches_a_fresh_host_at_every_endpoint() {
        let dir = TestDir::new("body-edit");
        dir.write(
            "main.rue",
            "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        dir.write("helper.rue", "pub fn value() -> i32 { 1 }\n");
        let mut retained = dir.open();
        let before = run_to_runnable(&mut retained);

        dir.write("helper.rue", "pub fn value() -> i32 { 2 }\n");
        retained.reobserve().unwrap();
        let after = run_to_runnable(&mut retained);
        let mut fresh = dir.open();
        let expected = run_to_runnable(&mut fresh);

        assert_ne!(before.elf, after.elf);
        assert_eq!(after.elf, expected.elf);
        assert_eq!(after.warnings, expected.warnings);
    }

    #[test]
    fn import_set_change_is_discovered_by_the_retained_host() {
        let dir = TestDir::new("import-set");
        dir.write(
            "main.rue",
            "const selected = @import(\"left.rue\");\nfn main() -> i32 { selected.value() }\n",
        );
        dir.write("left.rue", "pub fn value() -> i32 { 1 }\n");
        dir.write("right.rue", "pub fn value() -> i32 { 2 }\n");
        let mut retained = dir.open();
        let before = run_to_runnable(&mut retained);

        dir.write(
            "main.rue",
            "const selected = @import(\"right.rue\");\nfn main() -> i32 { selected.value() }\n",
        );
        retained.reobserve().unwrap();
        let after = run_to_runnable(&mut retained);
        let mut fresh = dir.open();
        let expected = run_to_runnable(&mut fresh);

        assert_ne!(before.elf, after.elf);
        assert_eq!(retained.source_snapshot().len(), 2);
        assert_eq!(after.elf, expected.elf);
    }

    #[test]
    fn manifest_policy_is_reloaded_before_reobservation() {
        let dir = TestDir::new("manifest-policy");
        let main = dir.write(
            "main.rue",
            "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        dir.write("helper.rue", "pub fn value() -> i32 { 1 }\n");
        let manifest = dir.write("sources.manifest", "main.rue\nhelper.rue\n");
        let mut host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: main.to_str().unwrap(),
            source_manifest_path: Some(manifest.to_str().unwrap()),
            std_root: None,
        })
        .unwrap();

        fs::write(&manifest, "main.rue\n").unwrap();
        match host.reobserve() {
            Err(SourceLoadError::Compiler { errors, .. }) => {
                let rendered = errors.to_string();
                assert!(rendered.contains("source manifest"), "{rendered}");
                assert!(rendered.contains("helper.rue"), "{rendered}");
            }
            other => panic!("expected a typed compiler policy error, got {other:?}"),
        }
    }

    #[test]
    fn reached_toolchain_demand_is_acquired_through_the_host() {
        let project = TestDir::new("toolchain-project");
        let stdlib = TestDir::new("toolchain-std");
        let main = project.write(
            "main.rue",
            "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }",
        );
        stdlib.write(
            "option.rue",
            "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }",
        );
        let std_root = fs::canonicalize(&stdlib.path).unwrap();
        let mut host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: main.to_str().unwrap(),
            source_manifest_path: None,
            std_root: Some(&std_root),
        })
        .unwrap();

        host.acquire_reached_toolchain_modules(&CompileOptions::default())
            .unwrap();

        assert_eq!(host.source_snapshot().len(), 2);
        assert!(
            host.accepted_reads()
                .iter()
                .any(|read| read.requested_path().ends_with("option.rue"))
        );
    }

    #[test]
    fn compiler_diagnostics_flow_through_the_host_endpoint() {
        let dir = TestDir::new("diagnostics");
        dir.write("main.rue", "fn main() -> i32 { missing_name }\n");
        let mut host = dir.open();

        let errors = match host.codegen_ready(&CompileOptions::default()) {
            Err(errors) => errors,
            Ok(_) => panic!("an undefined name must fail before codegen-ready"),
        };

        assert!(!errors.is_empty());
        assert!(errors.to_string().contains("missing_name"));
    }

    #[test]
    fn canceled_compile_does_not_produce_linked_bytes() {
        let dir = TestDir::new("canceled-compile");
        dir.write("main.rue", "fn main() -> i32 { 0 }\n");
        let mut host = dir.open();
        let cancellation = CompilationCancellation::new();
        cancellation.cancel();

        assert!(matches!(
            host.cancellable_executable_in_compile_scope(&CompileOptions::default(), cancellation,),
            CancellableCompileOutcome::Canceled
        ));
    }
}

#[cfg(test)]
mod test_candidate_acquisition_tests {
    use std::fs;
    use std::path::PathBuf;

    use rue_compiler::unstable::TestCandidateOutcome;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rue-{name}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Acquisition observes each declared candidate under the host's read
    /// policy, and the compiler reports exactly the unwired ones (ADR-0083 §1).
    ///
    /// Under a `--source-manifest` the policy order is the import loader's: a
    /// spelling the manifest does not declare is denied before the filesystem
    /// is probed, so an undeclared candidate is `Unreadable` whether or not a
    /// file exists there — and, being unreadable, it is reported. Without a
    /// manifest a missing file is observed `Absent` and stays silent. The root
    /// itself is in the closure and is never reported.
    #[test]
    fn acquires_declared_candidates_under_the_read_policy_and_reports_orphans() {
        let dir = scratch("test-candidate-acquisition");
        let write = |name: &str, text: &str| {
            fs::write(dir.join(name), text).unwrap();
        };
        write("main.rue", "fn main() -> i32 { 0 }\n");
        write("orphan_tests.rue", "test \"nothing imports this\" { }\n");
        write(
            "excluded_tests.rue",
            "test \"the manifest omits this\" { }\n",
        );
        write("sources.manifest", "main.rue\norphan_tests.rue\n");
        let root = dir.join("main.rue");
        let declared = [
            "main.rue".to_owned(),
            "orphan_tests.rue".to_owned(),
            "excluded_tests.rue".to_owned(),
            "absent_tests.rue".to_owned(),
        ];
        let outcome_of = |inventory: &TestCandidateInventory, path: &str| {
            inventory
                .candidates()
                .iter()
                .find(|candidate| candidate.path() == path)
                .map(|candidate| candidate.outcome().clone())
        };
        let rows_of = |report: &[rue_compiler::unstable::UnimportedTestFile]| {
            report
                .iter()
                .map(|row| (row.path.clone(), row.tests, row.parse_failed))
                .collect::<Vec<_>>()
        };

        // With a manifest: declared files read, undeclared ones denied unprobed.
        let manifest = dir.join("sources.manifest");
        let mut host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: root.to_str().unwrap(),
            source_manifest_path: Some(manifest.to_str().unwrap()),
            std_root: None,
        })
        .unwrap();
        let inventory = host.acquire_test_candidates(&declared).unwrap();
        assert!(matches!(
            outcome_of(&inventory, "main.rue"),
            Some(TestCandidateOutcome::Present(_))
        ));
        assert!(matches!(
            outcome_of(&inventory, "orphan_tests.rue"),
            Some(TestCandidateOutcome::Present(_))
        ));
        for denied in ["excluded_tests.rue", "absent_tests.rue"] {
            assert!(
                matches!(
                    outcome_of(&inventory, denied),
                    Some(TestCandidateOutcome::Unreadable(ref reason)) if reason.contains("source manifest")
                ),
                "{denied}: a candidate the manifest omits is denied, not probed: {:?}",
                outcome_of(&inventory, denied)
            );
        }
        // Candidate reads never join the accepted-read manifest.
        assert!(
            !host
                .accepted_reads()
                .iter()
                .any(|entry| entry.requested_path().ends_with("orphan_tests.rue")),
            "candidate acquisition must not record an accepted source read"
        );
        let report =
            rue_compiler::unstable::unimported_test_files(&mut host.state.session, &inventory)
                .unwrap();
        assert_eq!(
            rows_of(&report),
            vec![
                ("absent_tests.rue".to_owned(), 0, true),
                ("excluded_tests.rue".to_owned(), 0, true),
                ("orphan_tests.rue".to_owned(), 1, false),
            ]
        );

        // Without a manifest: a missing file is observed absent and stays silent.
        let mut host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: root.to_str().unwrap(),
            source_manifest_path: None,
            std_root: None,
        })
        .unwrap();
        let inventory = host
            .acquire_test_candidates(&[
                "orphan_tests.rue".to_owned(),
                "absent_tests.rue".to_owned(),
            ])
            .unwrap();
        assert!(matches!(
            outcome_of(&inventory, "absent_tests.rue"),
            Some(TestCandidateOutcome::Absent)
        ));
        let report =
            rue_compiler::unstable::unimported_test_files(&mut host.state.session, &inventory)
                .unwrap();
        assert_eq!(
            rows_of(&report),
            vec![("orphan_tests.rue".to_owned(), 1, false)]
        );
    }
}
