//! Retained hosts share root selections and keep invocation paths isolated.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rue_compiler::unstable::{
    CancellablePresentationOutcome, CancellableTestListingOutcome, CompilationCancellation,
    EndpointWork, PresentationBatchRequest, PresentationOutput, PresentationStage,
    QueryRuntimeMetrics, TestImage, TestListing,
};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileOutput, CompilerSessionConfig, OptLevel, RootSelection,
};
use rue_error::ErrorKind;

use crate::{FilesystemCompilerHost, HostOpenRequest, HostPathContext, SourceLoadError};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rue-host-workflow-{}-{timestamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn write(&self, source: &str) {
        fs::write(self.0.join("main.rue"), source).unwrap();
    }

    fn open(&self) -> FilesystemCompilerHost {
        self.open_with_workers(1)
    }

    fn open_with_workers(&self, workers: usize) -> FilesystemCompilerHost {
        let path_context = HostPathContext::from_working_directory(self.0.clone()).unwrap();
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source: "main.rue",
            source_manifest_path: None,
            std_root: None,
            compiler_config: CompilerSessionConfig::with_workers(workers).unwrap(),
            path_context: &path_context,
        })
        .unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(root_selection: RootSelection) -> CompileOptions {
    CompileOptions {
        opt_level: OptLevel::O0,
        root_selection,
        ..CompileOptions::default()
    }
}

fn acquire(host: &mut FilesystemCompilerHost, options: &CompileOptions) {
    host.acquire_reached_toolchain_modules(options)
        .expect("the fixture's reached toolchain demands must be satisfied");
}

fn air(host: &mut FilesystemCompilerHost) -> Result<PresentationOutput, CompileErrors> {
    let options = options(RootSelection::Executable);
    acquire(host, &options);
    let file_order = host
        .source_snapshot()
        .files()
        .map(|source| source.file_id)
        .collect::<Vec<_>>();
    host.present_many(PresentationBatchRequest {
        stages: &[PresentationStage::Air],
        options: &options,
        file_order: &file_order,
    })
    .map(|mut outputs| {
        assert_eq!(outputs.len(), 1);
        outputs.remove(0)
    })
}

fn listing(host: &mut FilesystemCompilerHost) -> Result<TestListing, CompileErrors> {
    let options = options(RootSelection::Tests);
    acquire(host, &options);
    host.test_inventory(&options)
}

fn image(host: &mut FilesystemCompilerHost) -> Result<TestImage, CompileErrors> {
    let options = options(RootSelection::Tests);
    acquire(host, &options);
    host.test_image_in_compile_scope(&options)
}

struct Build {
    output: CompileOutput,
    work: EndpointWork,
    queries: QueryRuntimeMetrics,
}

fn build(host: &mut FilesystemCompilerHost) -> Result<Build, CompileErrors> {
    // Acquisition can itself analyze bodies, so include it in the measured
    // request rather than counting only the final artifact lookup.
    let before = host.unstable_metrics().query_runtime();
    let options = options(RootSelection::Executable);
    acquire(host, &options);
    let codegen = host.codegen_ready(&options)?;
    let objects = host.objects_ready(codegen)?;
    let work = objects.unstable_work();
    let output = host.runnable_ready(objects)?;
    let queries = host
        .unstable_metrics()
        .query_runtime()
        .saturating_sub(before);
    Ok(Build {
        output,
        work,
        queries,
    })
}

fn assert_output_eq(actual: &CompileOutput, expected: &CompileOutput) {
    assert_eq!(actual.elf, expected.elf);
    assert_eq!(actual.warnings, expected.warnings);
}

fn assert_listing_eq(actual: &TestListing, expected: &TestListing) {
    assert_eq!(actual.inventory, expected.inventory);
    assert_eq!(
        actual.failure_diagnostics.as_slice(),
        expected.failure_diagnostics.as_slice()
    );
}

fn assert_image_eq(actual: &TestImage, expected: &TestImage) {
    assert_output_eq(&actual.output, &expected.output);
    assert_eq!(actual.inventory, expected.inventory);
    assert_eq!(
        actual.failure_diagnostics.as_slice(),
        expected.failure_diagnostics.as_slice()
    );
    assert_eq!(
        actual.compile_failures.len(),
        expected.compile_failures.len()
    );
    for (actual, expected) in actual
        .compile_failures
        .iter()
        .zip(&expected.compile_failures)
    {
        assert_eq!(actual.entry, expected.entry);
        assert_eq!(actual.errors.as_slice(), expected.errors.as_slice());
    }
}

fn assert_type_mismatch(errors: &CompileErrors) {
    assert!(
        errors
            .iter()
            .any(|error| matches!(error.kind, ErrorKind::TypeMismatch { .. })),
        "expected the fixture's type error, got {errors:?}"
    );
}

#[test]
fn retained_hosts_release_runtime_after_mixed_work_and_repair() {
    let project = Project::new();
    project.write(SHARED_PROGRAM);
    let (weak, held_air, held_listing, held_image, held_build, held_errors) = {
        let mut host = project.open_with_workers(2);
        let weak = host.unstable_query_runtime_weak();
        let held_air = air(&mut host).expect("AIR is available");
        let held_listing = listing(&mut host).expect("listing is available");
        let held_image = image(&mut host).expect("test image is available");
        let held_build = build(&mut host).expect("executable is available");

        project.write("fn main() -> i32 {\n    let value: i32 = \"broken\";\n    value\n}\n");
        host.reobserve().expect("the edited source is observed");
        let held_errors = build(&mut host)
            .err()
            .expect("the edited host reports its error");
        project.write(SHARED_PROGRAM);
        host.reobserve().expect("the repaired source is observed");
        assert!(build(&mut host).is_ok(), "the repaired host recovers");
        assert!(weak.is_alive(), "the active host owns a live runtime");
        (
            weak,
            held_air,
            held_listing,
            held_image,
            held_build,
            held_errors,
        )
    };
    assert!(
        !weak.is_alive(),
        "owned answers and diagnostics must not retain the dropped host's runtime"
    );
    // Keep every answer alive across teardown, then consume it against fresh
    // results. This checks both reclamation and the owned-response contract.
    assert_eq!(held_air, air(&mut project.open()).unwrap());
    assert_listing_eq(&held_listing, &listing(&mut project.open()).unwrap());
    assert_image_eq(&held_image, &image(&mut project.open()).unwrap());
    assert_output_eq(
        &held_build.output,
        &build(&mut project.open()).unwrap().output,
    );
    assert_type_mismatch(&held_errors);
}

#[test]
fn an_empty_host_releases_its_runtime() {
    let project = Project::new();
    project.write("");
    let weak = {
        let host = project.open();
        let weak = host.unstable_query_runtime_weak();
        assert!(weak.is_alive());
        weak
    };
    assert!(!weak.is_alive());
}

const SHARED_PROGRAM: &str = "\
fn shared(x: i32) -> i32 { x + 1 }\n\
fn main() -> i32 { shared(6) }\n\
test \"shared helper\" { let _value = shared(6); }\n";

#[test]
fn mixed_analysis_listing_image_build_reuses_shared_functions() {
    let project = Project::new();
    project.write(SHARED_PROGRAM);
    let mut retained = project.open();

    let first_air = air(&mut retained).unwrap();
    assert_eq!(first_air, air(&mut project.open()).unwrap());
    assert!(!first_air.as_str().contains("shared helper"));

    let listed = listing(&mut retained).unwrap();
    assert_listing_eq(&listed, &listing(&mut project.open()).unwrap());
    assert_eq!(listed.inventory.entries.len(), 1);
    assert_eq!(listed.inventory.entries[0].id, "main.rue::shared helper");
    assert_eq!(listed.inventory.entries[0].ordinal, 0);

    let test_image = image(&mut retained).unwrap();
    assert_image_eq(&test_image, &image(&mut project.open()).unwrap());

    let executable = build(&mut retained).unwrap();
    let fresh = build(&mut project.open()).unwrap();
    assert_output_eq(&executable.output, &fresh.output);
    let mut direct = project.open();
    let executable_options = options(RootSelection::Executable);
    acquire(&mut direct, &executable_options);
    assert_output_eq(
        &executable.output,
        &direct
            .executable_in_compile_scope(&executable_options)
            .unwrap(),
    );
    assert_eq!(air(&mut retained).unwrap(), first_air);

    // AIR already prepared main/shared's semantic bodies and CFGs. The image
    // then generated shared and the test, but never the source main. Of this
    // executable's two functions, shared is therefore the reused backend
    // unit and main is the newly computed one.
    assert_eq!(executable.work.semantic.computed, 0);
    assert_eq!(executable.work.semantic.reused, 2);
    assert_eq!(executable.work.cfg.computed, 0);
    assert_eq!(executable.work.cfg.reused, 2);
    assert_eq!(executable.work.codegen.computed, 1);
    assert_eq!(executable.work.codegen.reused, 1);
    // Internal linking uses structured codegen units without serializing
    // objects. The explicit object endpoint above is the first consumer of
    // these projections, so neither was available for reuse yet.
    assert_eq!(executable.work.object_projection.computed, 2);
    assert_eq!(executable.work.object_projection.reused, 0);
    assert!(executable.queries.reuses > 0);
    assert!(
        executable.queries.body_completions < fresh.queries.body_completions,
        "the whole warm request must compute fewer query bodies than a fresh build"
    );

    assert_image_eq(&image(&mut retained).unwrap(), &test_image);
    let rebuilt = build(&mut retained).unwrap();
    assert_output_eq(&rebuilt.output, &executable.output);
    assert_eq!(rebuilt.work.codegen.computed, 0);
    assert_eq!(rebuilt.work.codegen.reused, 2);
    assert_eq!(rebuilt.work.object_projection.computed, 0);
    assert_eq!(rebuilt.work.object_projection.reused, 2);
}

#[test]
fn mixed_requests_keep_broken_test_attribution_out_of_executables() {
    let project = Project::new();
    project.write(
        "\
fn shared(x: i32) -> i32 { x + 1 }\n\
fn main() -> i32 { shared(6) }\n\
test \"broken\" { let _bad: i32 = true; }\n\
test \"fine\" { let _value = shared(6); }\n",
    );
    let mut retained = project.open();

    assert_eq!(
        air(&mut retained).unwrap(),
        air(&mut project.open()).unwrap()
    );
    let listed = listing(&mut retained).unwrap();
    assert_listing_eq(&listed, &listing(&mut project.open()).unwrap());
    assert_eq!(listed.inventory.entries.len(), 2);
    assert_type_mismatch(&listed.failure_diagnostics);

    let test_image = image(&mut retained).unwrap();
    assert_image_eq(&test_image, &image(&mut project.open()).unwrap());
    assert_eq!(test_image.inventory, listed.inventory);
    assert_eq!(test_image.compile_failures.len(), 1);
    let failure = &test_image.compile_failures[0];
    assert_eq!(failure.entry.id, "main.rue::broken");
    assert_eq!(failure.entry.ordinal, 0);
    assert_eq!(test_image.inventory.entries[1].id, "main.rue::fine");
    assert_eq!(test_image.inventory.entries[1].ordinal, 1);
    assert_type_mismatch(&failure.errors);
    assert_eq!(test_image.failure_diagnostics.len(), 1);

    assert_output_eq(
        &build(&mut retained).unwrap().output,
        &build(&mut project.open()).unwrap().output,
    );
    assert_eq!(
        air(&mut retained).unwrap(),
        air(&mut project.open()).unwrap()
    );
    // Excluding broken from the runnable image must not change later roots.
    assert_listing_eq(&listing(&mut retained).unwrap(), &listed);
}

/// RUE-2174: acquisition, listing, and presentation each run under the
/// request's cancellation. A canceled request reports nothing — no partial
/// inventory, no diagnostics, no presentation — and the same retained host
/// then answers a live request exactly as a fresh host does.
#[test]
fn canceled_requests_report_nothing_and_the_host_recovers() {
    let project = Project::new();
    project.write(
        "\
fn shared(x: i32) -> i32 { x + 1 }\n\
fn main() -> i32 { shared(6) }\n\
test \"broken\" { let _bad: i32 = true; }\n\
test \"fine\" { let _value = shared(6); }\n",
    );
    let mut retained = project.open();
    let test_options = options(RootSelection::Tests);
    let executable_options = options(RootSelection::Executable);
    let file_order = retained
        .source_snapshot()
        .files()
        .map(|source| source.file_id)
        .collect::<Vec<_>>();
    const AIR: &[PresentationStage] = &[PresentationStage::Air];
    fn air_request<'a>(
        options: &'a CompileOptions,
        file_order: &'a [rue_compiler::FileId],
    ) -> PresentationBatchRequest<'a> {
        PresentationBatchRequest {
            stages: AIR,
            options,
            file_order,
        }
    }

    let canceled = CompilationCancellation::new();
    canceled.cancel();
    assert!(matches!(
        retained.acquire_reached_toolchain_modules_cancellable(&test_options, &canceled),
        Err(SourceLoadError::Superseded)
    ));
    assert!(matches!(
        retained.cancellable_test_inventory(&test_options, canceled.clone()),
        CancellableTestListingOutcome::Canceled
    ));
    assert!(matches!(
        retained.cancellable_present_many(
            air_request(&executable_options, &file_order),
            canceled.clone()
        ),
        CancellablePresentationOutcome::Canceled
    ));

    let live = CompilationCancellation::new();
    retained
        .acquire_reached_toolchain_modules_cancellable(&test_options, &live)
        .expect("a live acquisition settles");
    let listed = match retained.cancellable_test_inventory(&test_options, live.clone()) {
        CancellableTestListingOutcome::Completed(listing) => listing,
        CancellableTestListingOutcome::Errors(errors) => {
            panic!("a live listing analyzes: {errors:?}")
        }
        CancellableTestListingOutcome::Canceled => {
            panic!("a live token cannot report cancellation")
        }
    };
    assert_listing_eq(&listed, &listing(&mut project.open()).unwrap());
    assert_eq!(listed.inventory.entries.len(), 2);
    assert_eq!(listed.inventory.entries[0].id, "main.rue::broken");
    assert_eq!(listed.inventory.entries[1].id, "main.rue::fine");
    assert_type_mismatch(&listed.failure_diagnostics);

    retained
        .acquire_reached_toolchain_modules_cancellable(&executable_options, &live)
        .expect("a live acquisition settles");
    let presented = match retained
        .cancellable_present_many(air_request(&executable_options, &file_order), live)
    {
        CancellablePresentationOutcome::Completed(mut outputs) => outputs.pop().unwrap(),
        CancellablePresentationOutcome::Errors(errors) => {
            panic!("a live presentation renders: {errors:?}")
        }
        CancellablePresentationOutcome::Canceled => {
            panic!("a live token cannot report cancellation")
        }
    };
    assert_eq!(presented, air(&mut project.open()).unwrap());
    // The broken test's diagnostics belong to the listing alone: the
    // executable request over the same host neither sees them nor fails.
    assert_output_eq(
        &build(&mut retained).unwrap().output,
        &build(&mut project.open()).unwrap().output,
    );
}

#[test]
fn mixed_requests_keep_main_errors_out_of_tests_and_recover_after_edit() {
    let project = Project::new();
    project.write(
        "\
fn shared(x: i32) -> i32 { x + 1 }\n\
fn main() -> i32 { true }\n\
test \"shared helper\" { let _value = shared(6); }\n",
    );
    let mut retained = project.open();

    let errors = air(&mut retained).unwrap_err();
    assert_type_mismatch(&errors);
    assert_eq!(
        errors.as_slice(),
        air(&mut project.open()).unwrap_err().as_slice()
    );
    let listed = listing(&mut retained).unwrap();
    assert_listing_eq(&listed, &listing(&mut project.open()).unwrap());
    assert!(listed.failure_diagnostics.is_empty());
    let test_image = image(&mut retained).unwrap();
    assert_image_eq(&test_image, &image(&mut project.open()).unwrap());
    assert!(test_image.compile_failures.is_empty());
    let errors = build(&mut retained).err().expect("main is still ill-typed");
    assert_type_mismatch(&errors);
    let fresh_errors = build(&mut project.open())
        .err()
        .expect("fresh main is ill-typed too");
    assert_eq!(errors.as_slice(), fresh_errors.as_slice());
    assert_listing_eq(&listing(&mut retained).unwrap(), &listed);

    project.write(SHARED_PROGRAM);
    retained.reobserve().unwrap();
    assert_eq!(
        air(&mut retained).unwrap(),
        air(&mut project.open()).unwrap()
    );
    assert_listing_eq(
        &listing(&mut retained).unwrap(),
        &listing(&mut project.open()).unwrap(),
    );
    assert_image_eq(
        &image(&mut retained).unwrap(),
        &image(&mut project.open()).unwrap(),
    );
    assert_output_eq(
        &build(&mut retained).unwrap().output,
        &build(&mut project.open()).unwrap().output,
    );
}

#[test]
fn explicit_contexts_keep_relative_inputs_isolated_across_reobservation() {
    let projects = [Project::new(), Project::new()];
    for (index, project) in projects.iter().enumerate() {
        for directory in ["app", "control", "library"] {
            fs::create_dir(project.0.join(directory)).unwrap();
        }
        fs::write(
            project.0.join("app/main.rue"),
            "const helper = @import(\"helper.rue\");\n\
             fn main() -> i32 { let _ = @parse_i64(\"1\"); helper.value() }\n",
        )
        .unwrap();
        fs::write(
            project.0.join("app/helper.rue"),
            format!("pub fn value() -> i32 {{ {} }}\n", index + 1),
        )
        .unwrap();
        fs::write(
            project.0.join("app/orphan.rue"),
            "test \"not imported\" { }\n",
        )
        .unwrap();
        fs::write(
            project.0.join("library/option.rue"),
            "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }\n",
        )
        .unwrap();
        fs::write(
            project.0.join("control/sources.list"),
            "../app/main.rue\n../app/helper.rue\n../app/orphan.rue\n../library/option.rue\n",
        )
        .unwrap();
    }
    let open = |project: &Project| {
        let context = HostPathContext::from_working_directory(project.0.clone()).unwrap();
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source: "app/main.rue",
            source_manifest_path: Some("control/sources.list"),
            std_root: Some(Path::new("library")),
            compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
            path_context: &context,
        })
        .unwrap()
    };
    // Neither opening nor alternating the hosts changes the process cwd.
    // Each relative manifest entry is relative to control/, while candidates
    // are relative to app/, and the std root is relative to the invocation.
    let mut first = open(&projects[0]);
    let mut second = open(&projects[1]);
    let first_output = build(&mut first).unwrap().output;
    let second_output = build(&mut second).unwrap().output;
    assert_ne!(first_output.elf, second_output.elf);
    for (host, project) in [(&mut first, &projects[0]), (&mut second, &projects[1])] {
        let expected_std = fs::canonicalize(project.0.join("library")).unwrap();
        assert_eq!(host.discovery_context().std_root(), expected_std.to_str());
        assert!(host.accepted_reads().iter().any(|read| {
            read.requested_path() == expected_std.join("option.rue").to_str().unwrap()
        }));
        let candidates = host
            .acquire_test_candidates(&["orphan.rue".to_owned()])
            .unwrap();
        let report = host.unimported_test_files(&candidates).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].path, "orphan.rue");
        assert_eq!(report[0].tests, 1);
        assert!(!report[0].parse_failed);
    }

    fs::write(
        projects[0].0.join("app/helper.rue"),
        "pub fn value() -> i32 { 3 }\n",
    )
    .unwrap();
    first.reobserve().unwrap();
    second.reobserve().unwrap();
    let updated = build(&mut first).unwrap().output;
    assert_ne!(updated.elf, first_output.elf);
    assert_output_eq(&updated, &build(&mut open(&projects[0])).unwrap().output);
    assert_output_eq(&build(&mut second).unwrap().output, &second_output);

    // Refresh also resolves the manifest from the original invocation.
    fs::write(
        projects[0].0.join("control/sources.list"),
        "../app/main.rue\n",
    )
    .unwrap();
    let error = first.reobserve().unwrap_err();
    let crate::SourceLoadError::Compiler { errors, .. } = error else {
        panic!("a newly denied import must be a compiler policy error");
    };
    let rendered = errors.to_string();
    assert!(rendered.contains("source manifest"), "{rendered}");
    assert!(rendered.contains("helper.rue"), "{rendered}");
    // A read failure preserves the user's manifest spelling, independently
    // of the compiler's denied-import diagnostic above.
    fs::remove_file(projects[0].0.join("control/sources.list")).unwrap();
    let crate::SourceLoadError::Message(message) = first.reobserve().unwrap_err() else {
        panic!("a missing manifest must report its read failure");
    };
    assert!(message.contains("control/sources.list"), "{message}");
    second.reobserve().unwrap();
    assert_output_eq(&build(&mut second).unwrap().output, &second_output);
}

#[test]
fn explicit_context_preserves_empty_std_as_unconfigured() {
    let project = Project::new();
    project.write("fn main() -> i32 { 0 }\n");
    let context = HostPathContext::from_working_directory(project.0.clone()).unwrap();
    let mut host = FilesystemCompilerHost::open(HostOpenRequest {
        root_source: "main.rue",
        source_manifest_path: None,
        std_root: Some(Path::new("")),
        compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
        path_context: &context,
    })
    .unwrap();
    assert_eq!(host.discovery_context().std_root(), None);
    let expected = build(&mut project.open()).unwrap().output;
    assert_output_eq(&build(&mut host).unwrap().output, &expected);
    host.reobserve().unwrap();
    assert_eq!(host.discovery_context().std_root(), None);
    assert_output_eq(&build(&mut host).unwrap().output, &expected);
}

#[cfg(unix)]
#[test]
fn explicit_context_retains_requested_root_symlink_route() {
    let project = Project::new();
    for directory in ["link", "real"] {
        fs::create_dir(project.0.join(directory)).unwrap();
    }
    fs::write(
        project.0.join("real/main.rue"),
        "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
    )
    .unwrap();
    fs::write(
        project.0.join("real/helper.rue"),
        "pub fn value() -> i32 { 99 }\n",
    )
    .unwrap();
    fs::write(
        project.0.join("link/helper.rue"),
        "pub fn value() -> i32 { 1 }\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../real/main.rue", project.0.join("link/main.rue")).unwrap();
    let context = HostPathContext::from_working_directory(project.0.clone()).unwrap();
    let open = |root_source| {
        FilesystemCompilerHost::open(HostOpenRequest {
            root_source,
            source_manifest_path: None,
            std_root: None,
            compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
            path_context: &context,
        })
        .unwrap()
    };
    let mut retained = open("link/main.rue");
    assert_eq!(retained.root_path(), project.0.join("link/main.rue"));
    let root_read = retained
        .accepted_reads()
        .iter()
        .find(|read| read.requested_path().ends_with("link/main.rue"))
        .expect("the requested root spelling remains part of the observation");
    assert!(!root_read.symlink_route().is_empty());
    assert_eq!(
        root_read.canonical_path(),
        fs::canonicalize(project.0.join("real/main.rue"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    let before = build(&mut retained).unwrap().output;
    assert_ne!(
        before.elf,
        build(&mut open("real/main.rue")).unwrap().output.elf
    );
    fs::write(
        project.0.join("link/helper.rue"),
        "pub fn value() -> i32 { 2 }\n",
    )
    .unwrap();
    retained.reobserve().unwrap();
    let after = build(&mut retained).unwrap().output;
    assert_ne!(after.elf, before.elf);
    assert_output_eq(&after, &build(&mut open("link/main.rue")).unwrap().output);
}

#[cfg(unix)]
#[test]
fn manifest_and_std_capture_preserve_filesystem_symlink_parent_routes() {
    let project = Project::new();
    for directory in ["app", "physical/deep", "physical/toolchain"] {
        fs::create_dir_all(project.0.join(directory)).unwrap();
    }
    std::os::unix::fs::symlink("physical/deep", project.0.join("route")).unwrap();
    let main = project.0.join("app/main.rue");
    let option = project.0.join("physical/toolchain/option.rue");
    fs::write(&main, "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }\n").unwrap();
    fs::write(
        &option,
        "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }\n",
    )
    .unwrap();
    let manifest = project.0.join("physical/inputs.list");
    fs::write(
        &manifest,
        format!("{}\n{}\n", main.display(), option.display()),
    )
    .unwrap();
    let context = HostPathContext::from_working_directory(project.0.clone()).unwrap();
    let mut host = FilesystemCompilerHost::open(HostOpenRequest {
        root_source: "app/main.rue",
        source_manifest_path: Some("route/../inputs.list"),
        std_root: Some(Path::new("route/../toolchain")),
        compiler_config: CompilerSessionConfig::with_workers(1).unwrap(),
        path_context: &context,
    })
    .unwrap();
    let expected = build(&mut host).unwrap().output;
    assert_eq!(
        host.discovery_context().std_root(),
        project.0.join("physical/toolchain").to_str()
    );
    let observations = host.watch_inputs();
    assert!(!crate::watch_inputs_changed(&observations));
    assert!(
        observations
            .iter()
            .any(|input| { input.requested_path() == project.0.join("route/../inputs.list") })
    );
    fs::write(
        &manifest,
        format!(
            "# edited policy\n{}\n{}\n",
            main.display(),
            option.display()
        ),
    )
    .unwrap();
    assert!(crate::watch_inputs_changed(&observations));
    host.reobserve().unwrap();
    assert!(!crate::watch_inputs_changed(&host.watch_inputs()));
    assert_output_eq(&build(&mut host).unwrap().output, &expected);
    fs::remove_file(manifest).unwrap();
    let crate::SourceLoadError::Message(message) = host.reobserve().unwrap_err() else {
        panic!("refresh must reread the manifest through the captured route");
    };
    assert!(message.contains("route/../inputs.list"), "{message}");
}
