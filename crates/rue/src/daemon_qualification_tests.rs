//! Retained service requests must observe the same inputs as fresh requests.

use std::fs;

use super::*;

struct Project(tempfile::TempDir);

impl Project {
    fn new(source: &str) -> Self {
        let project = Self(tempfile::tempdir().unwrap());
        project.write("main.rue", source);
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.0.path().join(path)).unwrap();
    }

    fn request(&self) -> BuildRequest {
        BuildRequest {
            measure_performance: false,
            artifact: BuildKind::Executable,
            working_directory: self.0.path().display().to_string(),
            root_source: "main.rue".into(),
            output_path: "app".into(),
            source_manifest_path: None,
            test_candidates_path: None,
            std_root: None,
            workers: 1,
            target: rue_target::Target::host().unwrap().to_string(),
            opt_level: "O0".into(),
            preview_features: Vec::new(),
            link_archives: Vec::new(),
            error_format: DiagnosticFormat::Text,
            color: false,
        }
    }
}

#[track_caller]
fn fresh_parity(executor: &mut Executor, request: &BuildRequest, ready: bool) -> BuildOutput {
    let answer = executor.build(request, &CompilationCancellation::new());
    let fresh = Executor::new().build(request, &CompilationCancellation::new());
    assert!(
        answer.bytes == fresh.bytes,
        "retained/fresh executable bytes differ; lengths {}/{}; first mismatch {:?}",
        answer.bytes.len(),
        fresh.bytes.len(),
        answer
            .bytes
            .iter()
            .zip(&fresh.bytes)
            .position(|(left, right)| left != right)
    );
    assert_eq!(
        serde_json::to_value(&answer.result).unwrap(),
        serde_json::to_value(&fresh.result).unwrap(),
    );
    assert_eq!(
        matches!(answer.result, BuildResult::Ready { .. }),
        ready,
        "{:?}",
        answer.result
    );
    if !ready {
        assert!(
            matches!(answer.result, BuildResult::Rejected { .. }),
            "{:?}",
            answer.result
        );
    }
    answer
}

#[test]
fn daemon_pressure_eviction_reclaims_a_real_runtime_while_its_answer_is_held() {
    let project = Project::new("fn main() -> i32 { 42 }\n");
    let request = project.request();
    let mut executor = Executor::new();
    let held = fresh_parity(&mut executor, &request, true);
    let weak = executor
        .retained
        .as_ref()
        .unwrap()
        .host
        .unstable_query_runtime_weak();
    assert!(weak.is_alive());
    assert!(executor.retained_charge_bytes() > 0);

    // Exercise the production eviction branch with a deliberately small
    // policy; the socket-owner test separately verifies sample-before-trim
    // accounting and retention of peaks after these counters clear.
    executor.trim_over_budget(0, 0);
    assert!(
        !weak.is_alive(),
        "pressure eviction must reclaim the actual query graph"
    );
    assert_eq!(executor.retained_hosts(), 0);
    assert_eq!(executor.retained_charge_bytes(), 0);
    assert_eq!(executor.dependency_pins(), 0);
    let rebuilt = fresh_parity(&mut executor, &request, true);
    assert_eq!(held.bytes, rebuilt.bytes);
    assert_eq!(
        serde_json::to_value(held.result).unwrap(),
        serde_json::to_value(rebuilt.result).unwrap()
    );
}

#[test]
fn daemon_reobserves_missing_imports_and_source_manifest_policy() {
    let project = Project::new(
        "const helper = @import(\"helper.rue\"); fn main() -> i32 { helper.value() }\n",
    );
    project.write("helper.rue", "pub fn value() -> i32 { 21 }\n");
    project.write("sources.list", "main.rue\nhelper.rue\n");
    let mut request = project.request();
    request.source_manifest_path = Some("sources.list".into());
    let mut executor = Executor::new();
    let first = fresh_parity(&mut executor, &request, true);
    project.remove("helper.rue");
    fresh_parity(&mut executor, &request, false);
    project.write("helper.rue", "pub fn value() -> i32 { 42 }\n");
    let changed = fresh_parity(&mut executor, &request, true);
    assert_ne!(first.bytes, changed.bytes);

    project.write("sources.list", "main.rue\n");
    fresh_parity(&mut executor, &request, false);
    project.write("sources.list", "main.rue\nhelper.rue\n");
    let restored = fresh_parity(&mut executor, &request, true);
    assert_eq!(changed.bytes, restored.bytes);
    project.remove("sources.list");
    fresh_parity(&mut executor, &request, false);
    project.write("sources.list", "main.rue\nhelper.rue\n");
    assert_eq!(
        fresh_parity(&mut executor, &request, true).bytes,
        restored.bytes
    );
}

#[cfg(unix)]
#[test]
fn daemon_reobserves_std_contents_and_same_spelling_symlink_routes() {
    let project = Project::new("const std = @import(\"std\"); fn main() -> i32 { std.value() }\n");
    project.write("one/_std.rue", "pub fn value() -> i32 { 21 }\n");
    project.write("two/_std.rue", "pub fn value() -> i32 { 42 }\n");
    std::os::unix::fs::symlink("one", project.0.path().join("library")).unwrap();
    let mut request = project.request();
    request.std_root = Some("library".into());
    let mut executor = Executor::new();
    let first = fresh_parity(&mut executor, &request, true);
    project.write("one/_std.rue", "pub fn value() -> i32 { 22 }\n");
    let edited = fresh_parity(&mut executor, &request, true);
    assert_ne!(first.bytes, edited.bytes);
    let watched = executor.retained.as_ref().unwrap().host.watch_inputs();
    assert!(!rue_driver::watch_inputs_changed(&watched));
    project.remove("library");
    std::os::unix::fs::symlink("two", project.0.path().join("library")).unwrap();
    assert!(
        rue_driver::watch_inputs_changed(&watched),
        "watch and client publication must detect a std-root retarget before reobservation"
    );
    let rerouted = fresh_parity(&mut executor, &request, true);
    assert_ne!(edited.bytes, rerouted.bytes);
    project.remove("two/_std.rue");
    fresh_parity(&mut executor, &request, false);
    project.write("two/_std.rue", "pub fn value() -> i32 { 42 }\n");
    assert_eq!(
        fresh_parity(&mut executor, &request, true).bytes,
        rerouted.bytes
    );
    // A new physical target still passes the ordinary project/std overlap
    // gate. Its failure must leave a coherent host which can recover.
    project.remove("library");
    std::os::unix::fs::symlink(".", project.0.path().join("library")).unwrap();
    fresh_parity(&mut executor, &request, false);
    project.remove("library");
    std::os::unix::fs::symlink("two", project.0.path().join("library")).unwrap();
    assert_eq!(
        fresh_parity(&mut executor, &request, true).bytes,
        rerouted.bytes
    );
}

#[cfg(unix)]
#[test]
fn daemon_reobserves_std_routes_for_implicitly_acquired_modules() {
    let project = Project::new("fn main() -> i32 { let _ = @parse_i64(\"42\"); 42 }\n");
    for root in ["one", "two"] {
        project.write(
            &format!("{root}/option.rue"),
            "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }\n",
        );
    }
    std::os::unix::fs::symlink("one", project.0.path().join("library")).unwrap();
    let mut request = project.request();
    request.std_root = Some("library".into());
    let mut executor = Executor::new();
    let first = fresh_parity(&mut executor, &request, true);
    project.remove("library");
    // A dangling configured root reports a failure and can subsequently recover.
    std::os::unix::fs::symlink("missing", project.0.path().join("library")).unwrap();
    fresh_parity(&mut executor, &request, false);
    project.remove("library");
    std::os::unix::fs::symlink("two", project.0.path().join("library")).unwrap();
    let second = fresh_parity(&mut executor, &request, true);
    assert_eq!(first.bytes, second.bytes);
    assert_ne!(
        serde_json::to_value(first.result).unwrap(),
        serde_json::to_value(second.result).unwrap(),
        "equal executable bytes must still carry the newly accepted physical input records"
    );
}

#[cfg(unix)]
#[test]
fn daemon_std_directory_replaced_by_a_symlink_invalidates_equal_byte_inputs() {
    let project = Project::new("const std = @import(\"std\"); fn main() -> i32 { std.value() }\n");
    for root in ["library", "two"] {
        project.write(
            &format!("{root}/_std.rue"),
            "pub fn value() -> i32 { 42 }\n",
        );
    }
    let mut request = project.request();
    request.std_root = Some("library".into());
    let mut executor = Executor::new();
    let first = fresh_parity(&mut executor, &request, true);
    let inputs = executor.retained.as_ref().unwrap().host.watch_inputs();
    assert!(!rue_driver::watch_inputs_changed(&inputs));
    fs::rename(
        project.0.path().join("library"),
        project.0.path().join("old"),
    )
    .unwrap();
    std::os::unix::fs::symlink("two", project.0.path().join("library")).unwrap();
    assert!(rue_driver::watch_inputs_changed(&inputs));
    assert_eq!(
        fresh_parity(&mut executor, &request, true).bytes,
        first.bytes
    );
}

#[cfg(unix)]
#[test]
fn daemon_missing_std_alias_is_watched_until_retargeted_to_a_complete_library() {
    let project = Project::new("const std = @import(\"std\"); fn main() -> i32 { std.value() }\n");
    fs::create_dir(project.0.path().join("one")).unwrap();
    project.write("two/_std.rue", "pub fn value() -> i32 { 42 }\n");
    std::os::unix::fs::symlink("one", project.0.path().join("library")).unwrap();
    let mut request = project.request();
    request.std_root = Some("library".into());
    let mut executor = Executor::new();
    fresh_parity(&mut executor, &request, false);
    let inputs = executor.retained.as_ref().unwrap().host.watch_inputs();
    assert!(!rue_driver::watch_inputs_changed(&inputs));
    project.remove("library");
    std::os::unix::fs::symlink("two", project.0.path().join("library")).unwrap();
    assert!(rue_driver::watch_inputs_changed(&inputs));
    fresh_parity(&mut executor, &request, true);
}

#[cfg(unix)]
#[test]
fn daemon_reobserves_a_root_symlink_without_changing_the_request() {
    let project = Project::new("fn main() -> i32 { 0 }\n");
    project.write("one/main.rue", "fn main() -> i32 { 21 }\n");
    project.write("two/main.rue", "fn main() -> i32 { 42 }\n");
    std::os::unix::fs::symlink("one", project.0.path().join("route")).unwrap();
    let mut request = project.request();
    request.root_source = "route/main.rue".into();
    let mut executor = Executor::new();
    let first = fresh_parity(&mut executor, &request, true);
    project.remove("route");
    std::os::unix::fs::symlink("two", project.0.path().join("route")).unwrap();
    let second = fresh_parity(&mut executor, &request, true);
    assert_ne!(first.bytes, second.bytes);
    drop(executor);
    assert_eq!(
        fresh_parity(&mut Executor::new(), &request, true).bytes,
        second.bytes
    );
}

#[test]
fn daemon_reobserves_link_archive_creation_corruption_and_repair() {
    let project = Project::new("fn main() -> i32 { 42 }\n");
    let mut request = project.request();
    // The client anchors archive paths before crossing the service boundary.
    request.link_archives = vec![project.0.path().join("extra.a").display().to_string()];
    let mut executor = Executor::new();
    fresh_parity(&mut executor, &request, false);
    project.write("extra.a", "!<arch>\n");
    let first = fresh_parity(&mut executor, &request, true);
    project.write("extra.a", "invalid archive\n");
    fresh_parity(&mut executor, &request, false);
    project.write("extra.a", "!<arch>\n");
    assert_eq!(
        fresh_parity(&mut executor, &request, true).bytes,
        first.bytes
    );
}
