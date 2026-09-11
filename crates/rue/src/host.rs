use std::path::{Path, PathBuf};

use rue_compiler::unstable::TestCandidateInventory;
use rue_compiler::unstable::normalize_module_path;
use rue_compiler::unstable::{
    CancellableCompileOutcome, CancellablePresentationOutcome, CancellableTestImageOutcome,
    CancellableTestListingOutcome, CodegenReady, CompilationCancellation, ObjectsReady,
    PresentationBatchRequest, PresentationOutput, PresentationRequest, TestImage, TestListing,
    UnimportedTestFile, abort_import_input_request, cancellable_executable_in_compile_scope,
    cancellable_present_many, cancellable_test_image_in_compile_scope, cancellable_test_inventory,
    codegen_ready, executable_in_compile_scope, objects_ready, runnable_ready,
    test_image_in_compile_scope, test_inventory, unimported_test_files,
};
use rue_compiler::{
    AcceptedReadManifest, CompileErrors, CompileOptions, CompileOutput, CompilerSessionConfig,
    ImportDiscoveryContext, ImportDiscoveryStatus, ImportDiscoveryView, MultiErrorResult, RirView,
    SourceSnapshot,
};

use crate::source_loader::{
    AttemptedRead, ImportDiscoveryResult, SourceLoadError, SourceLoadRequest, WatchInput,
    acquire_reached_toolchain_modules, acquire_reached_toolchain_modules_cancellable, load,
    load_explicit_manifest, load_explicit_manifest_candidate, reload_from_filesystem,
};

/// Immutable filesystem configuration captured when a retained host opens.
#[derive(Clone, Debug)]
pub struct HostPathContext {
    working_directory: PathBuf,
}

impl HostPathContext {
    /// Capture the invocation directory once so retained hosts never resolve
    /// relative inputs against a later ambient working directory.
    pub fn capture() -> Result<Self, String> {
        std::env::current_dir()
            .map(|working_directory| Self { working_directory })
            .map_err(|error| format!("could not capture invocation directory: {error}"))
    }

    /// Construct a context for an embedding caller that already owns its path
    /// resolution boundary.
    pub fn from_working_directory(working_directory: impl Into<PathBuf>) -> Result<Self, String> {
        let working_directory = working_directory.into();
        if !working_directory.is_absolute() {
            return Err(format!(
                "invocation directory must be absolute: {}",
                working_directory.display()
            ));
        }
        Ok(Self { working_directory })
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    /// Resolve an invocation spelling without consulting the ambient process
    /// directory. The context is captured once at the request boundary.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        let anchored = if path.is_absolute() {
            path.to_owned()
        } else {
            self.working_directory.join(path)
        };
        PathBuf::from(normalize_module_path(&anchored.to_string_lossy()))
    }

    /// Anchor a client filesystem spelling to the captured invocation
    /// directory while preserving symlinks and `..` components. Compiler
    /// module identities use [`Self::resolve`]'s lexical normalization; link
    /// archives, output paths, and reproductions are ordinary filesystem
    /// arguments and must retain their requested route.
    pub fn anchor(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.working_directory.join(path)
        }
    }
}

pub struct HostOpenRequest<'a> {
    pub root_source: &'a str,
    pub source_manifest_path: Option<&'a str>,
    pub std_root: Option<&'a Path>,
    pub compiler_config: CompilerSessionConfig,
    pub path_context: &'a HostPathContext,
}

/// One canonical filesystem observer and retained compiler session.
pub struct FilesystemCompilerHost {
    state: ImportDiscoveryResult,
    path_context: HostPathContext,
}

impl FilesystemCompilerHost {
    /// Open a root source and drive parser-owned import discovery to closure.
    pub fn open(request: HostOpenRequest<'_>) -> Result<Self, SourceLoadError> {
        load(SourceLoadRequest {
            root_source: request.root_source,
            source_manifest_path: request.source_manifest_path,
            std_root: request.std_root,
            compiler_config: request.compiler_config,
            path_context: request.path_context,
        })
        .map(|state| Self {
            state,
            path_context: request.path_context.clone(),
        })
    }

    /// Open an opt-in explicit module manifest. The manifest is resolved from
    /// captured source bytes and exact import bindings; legacy discovery and
    /// its candidate filesystem probes remain the path used by [`Self::open`].
    pub fn open_explicit_manifest(
        request: HostOpenRequest<'_>,
        manifest_path: &str,
        manifest_std_root: Option<&Path>,
    ) -> Result<Self, SourceLoadError> {
        load_explicit_manifest(
            SourceLoadRequest {
                root_source: request.root_source,
                source_manifest_path: request.source_manifest_path,
                std_root: request.std_root,
                compiler_config: request.compiler_config,
                path_context: request.path_context,
            },
            manifest_path,
            manifest_std_root,
        )
        .map(|state| Self {
            state,
            path_context: request.path_context.clone(),
        })
    }

    /// Re-observe the exact accepted-read closure and publish its successor.
    pub fn reobserve(&mut self) -> Result<(), SourceLoadError> {
        if let Some(manifest) = self.state.explicit_manifest_path.clone() {
            let root_source = self.state.resolution.root_display_path.clone();
            let source_manifest = self
                .state
                .explicit_source_manifest_path
                .as_deref()
                .and_then(|path| path.to_str())
                .map(str::to_owned);
            let std_root = self.state.configured_std_root.clone();
            let candidate = load_explicit_manifest_candidate(
                SourceLoadRequest {
                    root_source: &root_source,
                    source_manifest_path: source_manifest.as_deref(),
                    std_root: None,
                    compiler_config: self.state.session.configuration().clone(),
                    path_context: &self.path_context,
                },
                &manifest.to_string_lossy(),
                std_root.as_deref(),
                &mut self.state.session,
                None,
            );
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(error) => {
                    let _ = abort_import_input_request(&mut self.state.session);
                    return Err(error);
                }
            };
            self.state.install_explicit_candidate(candidate);
            return Ok(());
        }
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
        if let Some(manifest) = self.state.explicit_manifest_path.clone() {
            if supersession() {
                return Err(SourceLoadError::Superseded);
            }
            let root_source = self.state.resolution.root_display_path.clone();
            let source_manifest = self
                .state
                .explicit_source_manifest_path
                .as_deref()
                .and_then(|path| path.to_str())
                .map(str::to_owned);
            let std_root = self.state.configured_std_root.clone();
            let candidate = load_explicit_manifest_candidate(
                SourceLoadRequest {
                    root_source: &root_source,
                    source_manifest_path: source_manifest.as_deref(),
                    std_root: None,
                    compiler_config: self.state.session.configuration().clone(),
                    path_context: &self.path_context,
                },
                &manifest.to_string_lossy(),
                std_root.as_deref(),
                &mut self.state.session,
                Some(supersession),
            );
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(error) => {
                    let _ = abort_import_input_request(&mut self.state.session);
                    return Err(error);
                }
            };
            if supersession() {
                // The close may already have committed this successor. Keep
                // session, snapshot, graph, and read manifests coherent when
                // the supersession signal arrives at that boundary.
                self.state.install_explicit_candidate(candidate);
                return Err(SourceLoadError::Superseded);
            }
            self.state.install_explicit_candidate(candidate);
            return Ok(());
        }
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
    /// promptly with [`SourceLoadError::Superseded`] once `cancellation` is
    /// canceled (RUE-1863, RUE-2174): between rounds, inside a round's reads,
    /// and inside the semantic probe that discovers each round's demands. A
    /// canceled acquisition never exposes partial state: a pre-commit abort
    /// keeps the prior state, while a cancellation observed after publication
    /// may retain that one coherent committed acquisition round.
    pub fn acquire_reached_toolchain_modules_cancellable(
        &mut self,
        options: &CompileOptions,
        cancellation: &CompilationCancellation,
    ) -> Result<(), SourceLoadError> {
        acquire_reached_toolchain_modules_cancellable(&mut self.state, options, cancellation)
    }

    pub fn source_snapshot(&self) -> &SourceSnapshot {
        &self.state.source_snapshot
    }

    /// The immutable compiler resources captured when this host was opened.
    pub fn compiler_configuration(&self) -> &CompilerSessionConfig {
        self.state.session.configuration()
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

    /// What the most recent observation attempt read, whether or not it
    /// committed.
    ///
    /// [`Self::watch_inputs`] answers "what is the accepted closure"; this
    /// answers the weaker "what did the loader last look at", which is the only
    /// account of a failed attempt's files, since a failure commits none of
    /// them (RUE-2103).
    pub fn attempted_reads(&self) -> &[AttemptedRead] {
        self.state.attempted_reads()
    }

    pub fn root_path(&self) -> &Path {
        &self.state.resolution.root_path
    }

    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        self.path_context.resolve(path)
    }

    pub fn anchor_path(&self, path: &Path) -> PathBuf {
        self.path_context.anchor(path)
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

    /// Return a non-owning query-runtime handle for lifecycle qualification;
    /// it grants no ability to issue compiler work.
    #[doc(hidden)]
    pub fn unstable_query_runtime_weak(&self) -> rue_compiler::unstable::QueryRuntimeLiveness {
        self.state.session.unstable_query_runtime_weak()
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

    /// Produce several unstable presentations from one rooted compile per
    /// stage family (RUE-1728), in the order the stages were named.
    pub fn present_many(
        &mut self,
        request: PresentationBatchRequest<'_>,
    ) -> MultiErrorResult<Vec<PresentationOutput>> {
        self.state.session.unstable_present_many(request)
    }

    /// Produce the presentations like [`Self::present_many`], under a caller's
    /// cancellation token (RUE-2174), so a retained host can abandon an
    /// analysis-only request during its CFG-side or backend work.
    pub fn cancellable_present_many(
        &mut self,
        request: PresentationBatchRequest<'_>,
        cancellation: CompilationCancellation,
    ) -> CancellablePresentationOutcome {
        cancellable_present_many(&mut self.state.session, request, cancellation)
    }

    /// Discovery's own diagnostics when this revision's import graph did not
    /// close valid, and nothing when it did.
    ///
    /// The compiler answers a compile request made against an uncommitted
    /// revision with its internal-input error (`E1400`), which describes the
    /// driver's mistake rather than the user's program. Every compile-scope
    /// entry point below refuses on this first, so no driver can reach the
    /// compiler with an unclosed graph: the unresolved `@import` that actually
    /// failed is what the caller receives, whichever endpoint it called.
    ///
    /// It is public because a driver also wants to ask before it starts
    /// preparing an output the revision will never produce — a program that
    /// will not resolve its imports is reported as that, not as whatever the
    /// destination preflight would have said about the path it was given
    /// (RUE-810).
    pub fn discovery_refusal(&self) -> Option<CompileErrors> {
        if self.discovery_status() == ImportDiscoveryStatus::ClosedValid {
            return None;
        }
        Some(self.discovery_revision().diagnostics().clone())
    }

    fn closed_discovery(&self) -> Result<(), CompileErrors> {
        match self.discovery_refusal() {
            Some(errors) => Err(errors),
            None => Ok(()),
        }
    }

    /// Preserve the command-line compiler's existing one-shot timed adapter.
    pub fn executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        self.closed_discovery()?;
        executable_in_compile_scope(&mut self.state.session, options)
    }

    pub fn cancellable_executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> CancellableCompileOutcome {
        if let Err(errors) = self.closed_discovery() {
            return CancellableCompileOutcome::Errors(errors);
        }
        cancellable_executable_in_compile_scope(&mut self.state.session, options, cancellation)
    }

    /// Analyze the request's test closure and publish its ordered inventory
    /// (ADR-0083 §2's `--list`), without codegen or linking, alongside the
    /// diagnostics of every body in the closure that failed to analyze.
    pub fn test_inventory(&mut self, options: &CompileOptions) -> MultiErrorResult<TestListing> {
        self.closed_discovery()?;
        test_inventory(&mut self.state.session, options)
    }

    /// Answer the listing like [`Self::test_inventory`], under a caller's
    /// cancellation token (RUE-2174), so a retained host can abandon a
    /// `rue test --list` request whose client has gone away.
    pub fn cancellable_test_inventory(
        &mut self,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> CancellableTestListingOutcome {
        if let Err(errors) = self.closed_discovery() {
            return CancellableTestListingOutcome::Errors(errors);
        }
        cancellable_test_inventory(&mut self.state.session, options, cancellation)
    }

    /// Link the test image for the request's closure and publish the inventory
    /// that assigned its dispatch ordinals, plus the tests the image excluded
    /// because their closures failed to analyze (ADR-0083 §3).
    pub fn test_image_in_compile_scope(
        &mut self,
        options: &CompileOptions,
    ) -> MultiErrorResult<TestImage> {
        self.closed_discovery()?;
        test_image_in_compile_scope(&mut self.state.session, options)
    }

    /// Link the test image like [`Self::test_image_in_compile_scope`], under a
    /// caller's cancellation token (RUE-2023).
    ///
    /// This is what makes a `rue test --watch` cycle abandonable: an edit
    /// landing while the image is being analyzed or linked cancels it, and the
    /// cycle reports nothing rather than diagnostics about source the user has
    /// already replaced.
    pub fn cancellable_test_image_in_compile_scope(
        &mut self,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> CancellableTestImageOutcome {
        if let Err(errors) = self.closed_discovery() {
            return CancellableTestImageOutcome::Errors(errors);
        }
        cancellable_test_image_in_compile_scope(&mut self.state.session, options, cancellation)
    }

    /// Report the declared candidates the compiled closure does not contain
    /// (ADR-0083 §1).
    ///
    /// Takes an inventory this same host acquired, so the report can never be
    /// made against a closure observed under a different read regime.
    pub fn unimported_test_files(
        &mut self,
        candidates: &TestCandidateInventory,
    ) -> Result<Vec<UnimportedTestFile>, CompileErrors> {
        unimported_test_files(&mut self.state.session, candidates)
    }

    /// How many caller-authored modules the published closure holds (RUE-1959).
    pub fn published_user_module_count(&self) -> usize {
        rue_compiler::unstable::published_user_module_count(&self.state.session)
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
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;

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
                compiler_config: CompilerSessionConfig::default(),
                path_context: &HostPathContext::capture().unwrap(),
            })
            .unwrap()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn path_context_requires_an_absolute_invocation_directory() {
        assert!(HostPathContext::from_working_directory("relative").is_err());
    }

    #[test]
    fn path_context_resolves_relative_paths_against_its_captured_directory() {
        let context = HostPathContext::from_working_directory("/tmp/rue-context").unwrap();
        assert_eq!(
            context.resolve(Path::new("./out/../program")),
            PathBuf::from("/tmp/rue-context/program")
        );
    }

    fn run_to_runnable(host: &mut FilesystemCompilerHost) -> CompileOutput {
        let options = CompileOptions::default();
        let codegen = host.codegen_ready(&options).unwrap();
        let objects = host.objects_ready(codegen).unwrap();
        host.runnable_ready(objects).unwrap()
    }

    fn write_two_module_manifest(dir: &TestDir, reverse_modules: bool) -> PathBuf {
        let modules = if reverse_modules {
            r#"{"module":"helper.rue","path":"helper.rue"},{"module":"main.rue","path":"main.rue"}"#
        } else {
            r#"{"module":"main.rue","path":"main.rue"},{"module":"helper.rue","path":"helper.rue"}"#
        };
        dir.write(
            "modules.json",
            &format!(
                r#"{{
  "version": 1,
  "root": "main.rue",
  "modules": [{modules}],
  "imports": [{{"importer":"project:main.rue","literal":"helper.rue","target":"project:helper.rue"}}],
  "std_requirements": []
}}"#
            ),
        )
    }

    fn open_two_module_explicit(dir: &TestDir) -> FilesystemCompilerHost {
        let root = dir.path.join("main.rue");
        let manifest = dir.path.join("modules.json");
        let context = HostPathContext::from_working_directory(dir.path.clone()).unwrap();
        FilesystemCompilerHost::open_explicit_manifest(
            HostOpenRequest {
                root_source: root.to_str().unwrap(),
                source_manifest_path: None,
                std_root: None,
                compiler_config: CompilerSessionConfig::default(),
                path_context: &context,
            },
            manifest.to_str().unwrap(),
            None,
        )
        .unwrap()
    }

    fn write_two_module_sources(dir: &TestDir, helper_value: i32) {
        dir.write(
            "main.rue",
            "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        dir.write(
            "helper.rue",
            &format!("pub fn value() -> i32 {{ {helper_value} }}\n"),
        );
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
    fn explicit_no_op_reobservation_reuses_the_retained_frontend() {
        let dir = TestDir::new("explicit-noop");
        write_two_module_sources(&dir, 7);
        write_two_module_manifest(&dir, false);
        let mut host = open_two_module_explicit(&dir);

        let first = run_to_runnable(&mut host);
        let before = host.unstable_metrics();
        host.reobserve().unwrap();
        let second = run_to_runnable(&mut host);
        let after = host.unstable_metrics();

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
    fn explicit_body_edit_matches_fresh_and_filesystem_hosts() {
        let dir = TestDir::new("explicit-body-edit");
        write_two_module_sources(&dir, 1);
        write_two_module_manifest(&dir, false);
        let mut retained = open_two_module_explicit(&dir);
        let before = run_to_runnable(&mut retained);

        write_two_module_sources(&dir, 2);
        retained.reobserve().unwrap();
        let after = run_to_runnable(&mut retained);
        let mut fresh_explicit = open_two_module_explicit(&dir);
        let expected = run_to_runnable(&mut fresh_explicit);
        let mut filesystem = dir.open();
        let ordinary = run_to_runnable(&mut filesystem);

        assert_ne!(before.elf, after.elf);
        assert_eq!(after.elf, expected.elf);
        assert_eq!(after.elf, ordinary.elf);
        assert_eq!(after.warnings, expected.warnings);
    }

    #[test]
    fn explicit_stale_keys_keep_last_good_state_then_recover_on_regeneration() {
        let dir = TestDir::new("explicit-stale-recover");
        write_two_module_sources(&dir, 3);
        let manifest = write_two_module_manifest(&dir, false);
        let mut retained = open_two_module_explicit(&dir);
        let committed = retained.source_snapshot().source_revision().clone();
        let expected = run_to_runnable(&mut retained);

        fs::write(
            &manifest,
            r#"{
  "version": 1, "root": "main.rue",
  "modules": [{"module":"main.rue","path":"main.rue"},{"module":"helper.rue","path":"helper.rue"}],
  "imports": [], "std_requirements": []
}"#,
        )
        .unwrap();
        let error = retained
            .reobserve()
            .expect_err("an omitted binding must reject the attempted revision");
        assert!(format!("{error:?}").contains("unused module entry"));
        assert_eq!(retained.source_snapshot().source_revision(), &committed);
        assert_eq!(
            retained.discovery_status(),
            ImportDiscoveryStatus::ClosedValid
        );
        assert!(retained.discovery_refusal().is_none());

        write_two_module_manifest(&dir, false);
        retained
            .reobserve()
            .expect("regenerating the exact manifest restores the closure");
        let recovered = run_to_runnable(&mut retained);
        assert_eq!(recovered.elf, expected.elf);
        assert_eq!(recovered.warnings, expected.warnings);
    }

    #[test]
    fn explicit_manifest_order_and_relocation_preserve_output_identity() {
        let dir = TestDir::new("explicit-order");
        write_two_module_sources(&dir, 4);
        write_two_module_manifest(&dir, false);
        let mut retained = open_two_module_explicit(&dir);
        let first = run_to_runnable(&mut retained);

        write_two_module_manifest(&dir, true);
        retained.reobserve().unwrap();
        let reordered = run_to_runnable(&mut retained);

        let relocated = TestDir::new("explicit-relocated");
        write_two_module_sources(&relocated, 4);
        write_two_module_manifest(&relocated, true);
        let mut moved = open_two_module_explicit(&relocated);
        let relocated_output = run_to_runnable(&mut moved);

        assert_eq!(first.elf, reordered.elf);
        assert_eq!(first.elf, relocated_output.elf);
        assert_eq!(first.warnings, reordered.warnings);
        assert_eq!(first.warnings, relocated_output.warnings);
    }

    #[test]
    fn explicit_superseding_reobserve_preserves_before_and_after_close_states() {
        let dir = TestDir::new("explicit-superseding");
        write_two_module_sources(&dir, 5);
        write_two_module_manifest(&dir, false);
        let mut host = open_two_module_explicit(&dir);
        let committed = host.source_snapshot().source_revision().clone();

        let staged = Rc::new(Cell::new(false));
        let staged_signal = Rc::clone(&staged);
        crate::source_loader::set_import_pre_close_hook(Some(Box::new(move || {
            staged_signal.set(true);
        })));
        let before = host
            .reobserve_superseding(&|| staged.get())
            .expect_err("a pre-close supersession must abort");
        crate::source_loader::set_import_pre_close_hook(None);
        assert!(matches!(before, SourceLoadError::Superseded));
        assert!(
            staged.get(),
            "the attempt must reach the pre-close checkpoint"
        );
        assert_eq!(host.source_snapshot().source_revision(), &committed);

        write_two_module_sources(&dir, 6);
        let superseded = Rc::new(Cell::new(false));
        let signal = Rc::clone(&superseded);
        crate::source_loader::set_explicit_candidate_close_hook(Some(Box::new(move || {
            signal.set(true);
        })));
        let after = host.reobserve_superseding(&|| superseded.get());
        crate::source_loader::set_explicit_candidate_close_hook(None);
        assert!(matches!(after, Err(SourceLoadError::Superseded)));
        assert!(
            host.source_snapshot()
                .files()
                .any(|source| source.source.contains("{ 6 }"))
        );
        assert_eq!(
            host.source_snapshot().source_revision(),
            host.discovery_revision().source_revision()
        );
        assert_eq!(host.accepted_reads().len(), host.source_snapshot().len());

        host.reobserve_superseding(&|| false)
            .expect("the coherent postclose successor must recover");
        let output = run_to_runnable(&mut host);
        let mut fresh = open_two_module_explicit(&dir);
        let expected = run_to_runnable(&mut fresh);
        assert_eq!(output.elf, expected.elf);
        assert_eq!(output.warnings, expected.warnings);
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
            compiler_config: CompilerSessionConfig::default(),
            path_context: &HostPathContext::capture().unwrap(),
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
            compiler_config: CompilerSessionConfig::default(),
            path_context: &HostPathContext::capture().unwrap(),
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

    /// A revision whose import graph did not close valid never reaches the
    /// compiler: every compile-scope entry point answers with discovery's own
    /// diagnostics instead of the compiler's internal-input error (RUE-1969).
    /// The watch loop compiles through the cancellable endpoint and the batch
    /// driver through the plain one, so both are pinned here.
    #[test]
    fn unclosed_discovery_reports_the_unresolved_import_at_every_entry_point() {
        let dir = TestDir::new("unclosed-discovery");
        dir.write(
            "main.rue",
            "const helper = @import(\"missing.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        let mut host = dir.open();
        assert_ne!(host.discovery_status(), ImportDiscoveryStatus::ClosedValid);

        let options = CompileOptions::default();
        let batch = match host.executable_in_compile_scope(&options) {
            Err(errors) => errors,
            Ok(_) => panic!("an unresolved import must not compile"),
        };
        let watch = match host
            .cancellable_executable_in_compile_scope(&options, CompilationCancellation::new())
        {
            CancellableCompileOutcome::Errors(errors) => errors,
            _ => panic!("an unresolved import must not compile"),
        };

        for rendered in [batch.to_string(), watch.to_string()] {
            assert!(rendered.contains("missing.rue"), "{rendered}");
            assert!(
                !rendered.contains("closed-valid import discovery revision"),
                "the driver's internal-input error must not reach a user: {rendered}"
            );
        }
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
            compiler_config: CompilerSessionConfig::default(),
            path_context: &HostPathContext::capture().unwrap(),
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
            compiler_config: CompilerSessionConfig::default(),
            path_context: &HostPathContext::capture().unwrap(),
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

    #[test]
    fn explicit_manifest_loads_and_reobserves_the_declared_closure() {
        let dir = scratch("explicit-manifest");
        let root = dir.join("main.rue");
        fs::write(
            &root,
            "const h = @import(\"helper.rue\");\nfn main() -> i32 { h.helper() }\n",
        )
        .unwrap();
        fs::write(dir.join("helper.rue"), "pub fn helper() -> i32 { 0 }\n").unwrap();
        let manifest = dir.join("modules.json");
        fs::write(
            &manifest,
            r#"{
  "version": 1,
  "root": "main.rue",
  "modules": [
    {"module": "main.rue", "path": "main.rue"},
    {"module": "helper.rue", "path": "helper.rue"}
  ],
  "imports": [
    {"importer": "project:main.rue", "literal": "helper.rue", "target": "project:helper.rue"}
  ],
  "std_requirements": []
}"#,
        )
        .unwrap();
        let context = HostPathContext::from_working_directory(dir.clone()).unwrap();
        let mut host = FilesystemCompilerHost::open_explicit_manifest(
            HostOpenRequest {
                root_source: root.to_str().unwrap(),
                source_manifest_path: None,
                std_root: None,
                compiler_config: CompilerSessionConfig::default(),
                path_context: &context,
            },
            manifest.to_str().unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(host.source_snapshot().len(), 2);
        assert_eq!(host.discovery_status(), ImportDiscoveryStatus::ClosedValid);
        let before = host.source_snapshot().source_revision().clone();
        let session_address = std::ptr::addr_of!(host.state.session);
        fs::write(dir.join("helper.rue"), "pub fn helper() -> i32 { 1 }\n").unwrap();
        host.reobserve().unwrap();
        assert_eq!(std::ptr::addr_of!(host.state.session), session_address);
        assert_ne!(host.source_snapshot().source_revision(), &before);
        assert_eq!(host.discovery_status(), ImportDiscoveryStatus::ClosedValid);
    }

    #[test]
    fn explicit_manifest_reobserve_rejects_stale_keys_and_keeps_last_good_state() {
        let dir = scratch("explicit-manifest-stale-keys");
        let root = dir.join("main.rue");
        fs::write(
            &root,
            "const h = @import(\"helper.rue\");\nfn main() -> i32 { h.helper() }\n",
        )
        .unwrap();
        fs::write(dir.join("helper.rue"), "pub fn helper() -> i32 { 0 }\n").unwrap();
        let manifest = dir.join("modules.json");
        let valid = r#"{
  "version": 1, "root": "main.rue",
  "modules": [{"module":"main.rue","path":"main.rue"},{"module":"helper.rue","path":"helper.rue"}],
  "imports": [{"importer":"project:main.rue","literal":"helper.rue","target":"project:helper.rue"}],
  "std_requirements": []
}"#;
        fs::write(&manifest, valid).unwrap();
        let context = HostPathContext::from_working_directory(dir.clone()).unwrap();
        let mut host = FilesystemCompilerHost::open_explicit_manifest(
            HostOpenRequest {
                root_source: root.to_str().unwrap(),
                source_manifest_path: None,
                std_root: None,
                compiler_config: CompilerSessionConfig::default(),
                path_context: &context,
            },
            manifest.to_str().unwrap(),
            None,
        )
        .unwrap();
        let committed = host.source_snapshot().source_revision().clone();
        fs::write(
            &manifest,
            valid.replace(
                r#"[{"importer":"project:main.rue","literal":"helper.rue","target":"project:helper.rue"}]"#,
                "[]",
            ),
        )
        .unwrap();
        let error = host.reobserve().expect_err("omitted key is stale input");
        assert!(format!("{error:?}").contains("unused module entry"));
        assert_eq!(host.source_snapshot().source_revision(), &committed);

        let extra = r#"{
  "version": 1, "root": "main.rue",
  "modules": [{"module":"main.rue","path":"main.rue"},{"module":"helper.rue","path":"helper.rue"}],
  "imports": [
    {"importer":"project:main.rue","literal":"helper.rue","target":"project:helper.rue"},
    {"importer":"project:main.rue","literal":"other.rue","target":null}
  ],
  "std_requirements": []
}"#;
        fs::write(&manifest, extra).unwrap();
        let error = host.reobserve().expect_err("extra key is stale input");
        assert!(format!("{error:?}").contains("exactly cover"));
        assert_eq!(host.source_snapshot().source_revision(), &committed);
    }
}
