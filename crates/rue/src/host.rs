use std::path::Path;

use rue_compiler::unstable::{
    CodegenReady, ObjectsReady, PresentationOutput, PresentationRequest, codegen_ready,
    executable_in_compile_scope, objects_ready, runnable_ready,
};
use rue_compiler::{
    AcceptedReadManifest, CompileOptions, CompileOutput, ImportDiscoveryContext,
    ImportDiscoveryStatus, ImportDiscoveryView, MultiErrorResult, RirView, SourceSnapshot,
};

use crate::source_loader::{
    ImportDiscoveryResult, SourceLoadError, SourceLoadRequest, acquire_reached_toolchain_modules,
    load, reload_from_filesystem,
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
        reload_from_filesystem(&mut self.state)
    }

    /// Satisfy compiler-issued reached-body toolchain demands for this revision.
    pub fn acquire_reached_toolchain_modules(
        &mut self,
        options: &CompileOptions,
    ) -> Result<(), SourceLoadError> {
        acquire_reached_toolchain_modules(&mut self.state, options)
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

    pub fn root_path(&self) -> &Path {
        &self.state.resolution.root_path
    }

    pub fn discovery_context(&self) -> &ImportDiscoveryContext {
        &self.state.resolution.context
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
}
