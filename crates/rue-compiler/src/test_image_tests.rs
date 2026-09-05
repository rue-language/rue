//! End-to-end coverage for the test image and its exec contract (ADR-0083 §3).
//!
//! These link a real image through the ADR-0061 facade and run it the way the
//! runner will: `argv = ["rue-test", "<16 hex digits>"]`, `envp =
//! ["RUE_TEST=1"]`, a private working directory, `/dev/null` on stdin, and the
//! write end of the failure-channel pipe on descriptor 3. Nothing below reads
//! compiler internals — the assertions are on exit status, the two byte
//! streams, and the channel — because those are what the runner will observe.
//!
//! They are host-native (`platform_native_`, `#[ignore]`) and run through
//! `rue-compiler-platform-native-test`: a linked image only executes on the
//! target it was built for, and the default target is the host.

#![cfg(all(test, unix))]

use crate::{CompileOptions, CompilerSession, RootSelection};

/// The exact completion frame the dispatcher's epilogue writes.
const COMPLETE_FRAME: &str = "{\"record\":\"complete\",\"schema\":\"1.0\"}\n";

/// The pinned malformed-selector diagnostic.
const USAGE_MESSAGE: &str = "rue-test: expected one 16-hex-digit test selector\n";

/// The host's `exit` syscall number, matching `std._exit_syscall_number`.
///
/// Spelled here rather than imported so the fixture needs no standard library:
/// a test image's closure is whatever its tests import, and this one imports
/// nothing.
const EXIT_SYSCALL: u64 = if cfg!(target_os = "macos") {
    1
} else if cfg!(target_arch = "aarch64") {
    93
} else {
    60
};

/// What one dispatched test process produced.
struct DispatchedRun {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    channel: String,
}

fn test_options() -> CompileOptions {
    CompileOptions {
        root_selection: RootSelection::Tests,
        ..CompileOptions::default()
    }
}

/// Link the three-test fixture and return its image plus its inventory.
///
/// One image serves every case below: the marginal cost of another verdict is
/// one more `test` item, while another image is another link.
fn dispatch_fixture() -> (Vec<u8>, crate::unstable::TestInventory) {
    let source = crate::SourceSnapshot::single(
        "main.rue",
        &format!(
            "test \"alpha passes\" {{ println(\"ran alpha\"); }}\n\
             test \"bravo exits early\" {{ checked {{ @syscall({EXIT_SYSCALL}, 0); }}; }}\n\
             test \"charlie traps\" {{ @panic(\"boom\"); }}\n"
        ),
    )
    .unwrap();
    let mut session = CompilerSession::new();
    crate::test_support::TestDiscoveryHost::new(&source)
        .unwrap()
        .drive(&mut session)
        .unwrap();
    let (image, inventory) =
        crate::unstable::test_image_in_compile_scope(&mut session, &test_options())
            .expect("the test image links");
    (image.elf, inventory)
}

/// Run `image` under the exec contract with `selector`.
fn run_test_image(image: &[u8], label: &str, selector: &str) -> DispatchedRun {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_IMAGE: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT_IMAGE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rue-test-image-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::write(&path, image).expect("write linked test image");
    let mut permissions = std::fs::metadata(&path)
        .expect("read linked test image metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make the linked test image runnable");
    #[cfg(target_os = "macos")]
    {
        let signing = std::process::Command::new("codesign")
            .args([
                "-f",
                "-s",
                "-",
                "--identifier",
                "dev.rue-lang.test-image",
                "--timestamp=none",
            ])
            .arg(&path)
            .output()
            .expect("run ad-hoc codesign for the linked test image");
        assert!(
            signing.status.success(),
            "codesign linked test image: {}",
            String::from_utf8_lossy(&signing.stderr)
        );
    }

    // The failure channel. The runner inherits its write end on descriptor 3;
    // this stands in for that, including the parent holding the read end open
    // until the child exits.
    let mut channel_fds = [0 as libc::c_int; 2];
    // SAFETY: `channel_fds` is a two-element array, exactly what `pipe` writes.
    assert_eq!(unsafe { libc::pipe(channel_fds.as_mut_ptr()) }, 0);
    let [channel_read, channel_write] = channel_fds;

    let scratch = tempfile::tempdir().expect("per-test scratch directory");
    let mut command = std::process::Command::new(&path);
    command
        .arg0("rue-test")
        .arg(selector)
        .env_clear()
        .env("RUE_TEST", "1")
        .current_dir(scratch.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: the hook calls only `dup2` and `close`, both async-signal-safe,
    // and touches no memory the fork shares with another thread.
    unsafe {
        command.pre_exec(move || {
            // The read end goes first. `pipe` hands back the lowest free
            // descriptors, so it is frequently descriptor 3 itself — closing it
            // after the `dup2` below would close the write end that had just
            // replaced it, and the channel would silently carry nothing.
            libc::close(channel_read);
            if libc::dup2(channel_write, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if channel_write != 3 {
                libc::close(channel_write);
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn the dispatched test process");
    // The parent's own write end must go before the read reaches end of stream.
    // SAFETY: this descriptor is owned here and closed exactly once.
    unsafe { libc::close(channel_write) };
    let output = child
        .wait_with_output()
        .expect("collect the dispatched test process output");
    // SAFETY: `channel_read` is a live descriptor this call takes ownership of.
    let mut reader = unsafe { std::fs::File::from_raw_fd(channel_read) };
    let mut channel = Vec::new();
    reader
        .read_to_end(&mut channel)
        .expect("drain the failure channel");
    std::fs::remove_file(&path).expect("remove the linked test image");

    DispatchedRun {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        channel: String::from_utf8_lossy(&channel).into_owned(),
    }
}

/// A selected test runs, and only the dispatcher's epilogue writes the
/// completion frame (ADR-0083 §3).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_test_image_runs_the_selected_test_and_completes() {
    let (image, inventory) = dispatch_fixture();
    assert_eq!(
        inventory
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "main.rue::alpha passes",
            "main.rue::bravo exits early",
            "main.rue::charlie traps",
        ]
    );

    let run = run_test_image(&image, "pass", "0000000000000000");
    assert_eq!(run.status, Some(0), "{run:?}", run = run.stderr);
    assert_eq!(run.stdout, "ran alpha\n");
    assert_eq!(run.stderr, "");
    assert_eq!(run.channel, COMPLETE_FRAME);
}

/// A body that exits before returning writes no completion frame, which is the
/// evidence the runner turns into the `incomplete` verdict (ADR-0083 §3).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_an_early_exit_produces_no_completion_frame() {
    let (image, _) = dispatch_fixture();
    let run = run_test_image(&image, "early-exit", "0000000000000001");
    assert_eq!(run.status, Some(0), "an early `exit(0)` still exits zero");
    assert_eq!(
        run.channel, "",
        "exit 0 with no completion frame is exactly the `incomplete` hazard"
    );
}

/// A trapping body takes the ordinary runtime abort path, and reports it on
/// the channel with the site of the `@panic` that caused it (RUE-2019).
///
/// The record's kind and message are exactly what the runner would otherwise
/// have read off stderr, so what the frame adds is the location — without it
/// the failure could only name the `test` declaration's header. The pinned
/// stderr line and the exit status are unchanged (spec 4.13:5c).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_trapping_test_reports_its_site_and_exits_101() {
    let (image, _) = dispatch_fixture();
    let run = run_test_image(&image, "trap", "0000000000000002");
    assert_eq!(run.status, Some(101));
    assert_eq!(run.stderr, "panic: boom\n");
    assert_eq!(
        run.channel,
        concat!(
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"trap:panic\",",
            "\"message\":\"panic: boom\",",
            "\"location\":{\"file\":\"main.rue\",\"line\":3,\"column\":24}}\n",
        )
    );
}

/// Every malformed or out-of-range selector is one pinned diagnostic and exit
/// 2 (ADR-0083 §3): a dispatcher that cannot read its own argv is a runner
/// error, not a test failure.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_bad_selector_is_the_pinned_usage_error() {
    let (image, _) = dispatch_fixture();
    for (label, selector) in [
        ("out-of-range", "0000000000000003"),
        ("saturated", "ffffffffffffffff"),
        ("too-short", "0"),
        ("non-hex", "00000000000000zz"),
        ("uppercase", "000000000000000A"),
    ] {
        let run = run_test_image(&image, label, selector);
        assert_eq!(run.status, Some(2), "{label}: {}", run.stderr);
        assert_eq!(run.stderr, USAGE_MESSAGE, "{label}");
        assert_eq!(run.stdout, "", "{label}");
        assert_eq!(run.channel, "", "{label}");
    }
}

/// The test-visible inventory after normalization is the pinned one
/// (ADR-0083 §3): one argument spelled `rue-test`, and one environment entry.
///
/// The fixture reads the process accessors directly rather than through
/// `std.env`, for the same reason the dispatcher does: a test image's closure
/// is whatever its tests import, and this one imports nothing. It reports by
/// trapping, so a wrong inventory names which field was wrong.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_test_observes_the_pinned_process_inventory() {
    let source = crate::SourceSnapshot::single(
        "main.rue",
        "fn byte_at(p: ptr mut u8, i: u64) -> u64 {\n\
        \x20   let b: u8 = checked { @ptr_read(@ptr_offset(p, i)) };\n\
        \x20   @intCast(b)\n\
         }\n\
         test \"observes its inventory\" {\n\
        \x20   let argc: u64 = checked { @arg_count() };\n\
        \x20   if argc != 1 { @panic(\"arg_count\"); }\n\
        \x20   let envc: u64 = checked { @env_count() };\n\
        \x20   if envc != 1 { @panic(\"env_count\"); }\n\
        \x20   let name_len: u64 = checked { @arg_len(0) };\n\
        \x20   if name_len != 8 { @panic(\"arg(0) length\"); }\n\
        \x20   let name: ptr mut u8 = checked { @arg_ptr(0) };\n\
        \x20   if byte_at(name, 0) != 114 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 1) != 117 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 2) != 101 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 3) != 45 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 4) != 116 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 5) != 101 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 6) != 115 { @panic(\"arg(0) bytes\"); }\n\
        \x20   if byte_at(name, 7) != 116 { @panic(\"arg(0) bytes\"); }\n\
        \x20   let entry_len: u64 = checked { @env_len(0) };\n\
        \x20   if entry_len != 10 { @panic(\"env(0) length\"); }\n\
        \x20   println(\"inventory ok\");\n\
         }\n",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    crate::test_support::TestDiscoveryHost::new(&source)
        .unwrap()
        .drive(&mut session)
        .unwrap();
    let (image, inventory) =
        crate::unstable::test_image_in_compile_scope(&mut session, &test_options())
            .expect("the test image links");
    assert_eq!(inventory.entries.len(), 1);

    let run = run_test_image(&image.elf, "inventory", "0000000000000000");
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(
        run.stdout, "inventory ok\n",
        "the selector must be invisible to the test: {}",
        run.stderr
    );
    assert_eq!(run.channel, COMPLETE_FRAME);
}

// ===========================================================================
// `?` in a test body (ADR-0083 §1, spec 6.7:13 - 6.7:17).
//
// The session tests pin the lowered shape; these pin what the runner sees. One
// image serves every verdict, for the same reason the dispatch fixture does:
// another `test` item is cheap, another link is not.
// ===========================================================================

/// The one image every `?` case below is dispatched from, with its inventory.
fn try_fixture() -> (Vec<u8>, crate::unstable::TestInventory) {
    let source = crate::test_body_try_tests::trusted_snapshot(TRY_FIXTURE_SOURCE);
    let mut session = CompilerSession::new();
    crate::test_support::TestDiscoveryHost::new(&source)
        .unwrap()
        .drive(&mut session)
        .unwrap();
    let (image, inventory) = crate::unstable::test_image_in_compile_scope(
        &mut session,
        &crate::test_body_try_tests::test_options(),
    )
    .expect("the test image links");
    (image.elf, inventory)
}

/// The 1-based line of the one occurrence of `needle` in the fixture, so a
/// location assertion names the `?` site by what is written there rather than
/// by a number that drifts whenever the fixture gains a line.
fn fixture_line(needle: &str) -> u32 {
    let lines: Vec<_> = TRY_FIXTURE_SOURCE
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .collect();
    assert_eq!(lines.len(), 1, "`{needle}` must occur once in the fixture");
    // `trusted_snapshot` publishes the source verbatim, and the raw literal
    // opens with a newline, so the literal's line numbering is the module's.
    u32::try_from(lines[0].0 + 1).expect("a fixture line number fits u32")
}

const TRY_FIXTURE_SOURCE: &str = r#"
const opt = @import("std/option.rue");
const res = @import("std/result.rue");
const sb = @import("std/strbuf.rue");

pub enum Code {
    Missing,
    Invalid(i32, sb.StrBuf),
}

pub struct Detail {
    code: i32,
    retryable: bool,
}

pub struct Guard {
    tag: i32,
}

drop fn Guard(self) {
    println("guard ran");
}

fn absent() -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.None
}

fn unit_variant() -> res.Result(i64, Code) {
    let R = res.Result(i64, Code);
    R.Err(Code.Missing)
}

fn payload_variant() -> res.Result(i64, Code) {
    let R = res.Result(i64, Code);
    R.Err(Code.Invalid(0 - 7, sb.owned("bad")))
}

fn detail_error() -> res.Result(i64, Detail) {
    let R = res.Result(i64, Detail);
    R.Err(Detail { code: 9, retryable: true })
}

fn oversized() -> res.Result(i64, sb.StrBuf) {
    let R = res.Result(i64, sb.StrBuf);
    R.Err(sb.repeated(120, 5000))
}

fn ok_value() -> res.Result(i64, Detail) {
    let R = res.Result(i64, Detail);
    R.Ok(5)
}

test "option none" {
    let v = absent()?;
    println("not reached");
}

test "enum unit variant" {
    let v = unit_variant()?;
    println("not reached");
}

test "enum payload variant" {
    let v = payload_variant()?;
    println("not reached");
}

test "struct error" {
    let v = detail_error()?;
    println("not reached");
}

test "oversized payload" {
    let v = oversized()?;
    println("not reached");
}

test "succeeds" {
    let v = ok_value()?;
    @assert(v == 5);
    println("five");
}

test "skips destructors" {
    let guard = Guard { tag: 1 };
    let v = detail_error()?;
    println("not reached");
}
"#;

/// The selector for `name` in the fixture's inventory order.
fn selector_for(inventory: &crate::unstable::TestInventory, name: &str) -> String {
    let ordinal = inventory
        .entries
        .iter()
        .position(|entry| entry.id.ends_with(name))
        .unwrap_or_else(|| panic!("the fixture declares a test named `{name}`"));
    format!("{ordinal:016x}")
}

/// The `payload` field of the one `failure` frame on the channel.
fn failure_payload(channel: &str) -> String {
    let frame = channel
        .lines()
        .find(|line| line.contains("\"record\":\"failure\""))
        .unwrap_or_else(|| panic!("the channel carries one failure frame: {channel:?}"));
    let marker = "\"payload\":\"";
    let start = frame
        .find(marker)
        .expect("a failure frame carries a payload")
        + marker.len();
    let rest = &frame[start..];
    let end = rest.find("\"}").expect("the payload field is terminated");
    rest[..end].to_owned()
}

/// The `line` and `column` of the one `failure` frame on the channel.
fn failure_location(channel: &str) -> (String, u32, u32) {
    let frame = channel
        .lines()
        .find(|line| line.contains("\"record\":\"failure\""))
        .unwrap_or_else(|| panic!("the channel carries one failure frame: {channel:?}"));
    let field = |name: &str| {
        let marker = format!("\"{name}\":");
        let start = frame.find(&marker).expect("the location field is present") + marker.len();
        frame[start..]
            .split(|byte: char| byte == ',' || byte == '}')
            .next()
            .expect("a field has a value")
            .trim_matches('"')
            .to_owned()
    };
    (
        field("file"),
        field("line").parse().expect("line is a number"),
        field("column").parse().expect("column is a number"),
    )
}

/// A failing `?` traps at its site: exit 101, the pinned stderr message, and one
/// `unhandled_error` frame naming the site (spec 6.7:14).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_failing_test_body_question_reports_and_traps() {
    let (image, inventory) = try_fixture();
    let run = run_test_image(&image, "try-none", &selector_for(&inventory, "option none"));
    assert_eq!(run.status, Some(101), "{}", run.stderr);
    assert_eq!(run.stderr, "panic: unhandled error\n");
    assert_eq!(run.stdout, "", "the code after a failing `?` does not run");
    assert!(
        run.channel.contains("\"kind\":\"unhandled_error\""),
        "{:?}",
        run.channel
    );
    assert!(
        run.channel.contains("\"message\":\"unhandled error\""),
        "{:?}",
        run.channel
    );
    let (file, line, column) = failure_location(&run.channel);
    assert!(file.ends_with("main.rue"), "unexpected file: {file:?}");
    assert_eq!(
        (line, column),
        (fixture_line("absent()?"), 13),
        "the frame names the `?` site, not the test or the callee: {:?}",
        run.channel
    );
}

/// The rendered payload, one shape per rule of spec 6.7:15.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_the_reported_payload_is_the_rendered_error() {
    let (image, inventory) = try_fixture();
    for (name, expected) in [
        ("option none", "None"),
        ("enum unit variant", "Missing"),
        ("enum payload variant", "Invalid(-7, bad)"),
        ("struct error", "{ code: 9, retryable: true }"),
    ] {
        let run = run_test_image(&image, name, &selector_for(&inventory, name));
        assert_eq!(
            run.status,
            Some(101),
            "{name}: stderr={:?} stdout={:?} channel={:?}",
            run.stderr,
            run.stdout,
            run.channel
        );
        assert!(
            run.channel.contains("\"record\":\"failure\""),
            "{name}: no failure frame. stderr={:?} stdout={:?} channel={:?}",
            run.stderr,
            run.stdout,
            run.channel
        );
        assert_eq!(
            failure_payload(&run.channel),
            expected,
            "{name}: {:?}",
            run.channel
        );
    }
}

/// The rendering is bounded, and a rendering that hit the bound says so
/// (spec 6.7:15).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_an_oversized_payload_is_truncated_with_its_marker() {
    let (image, inventory) = try_fixture();
    let run = run_test_image(
        &image,
        "try-truncate",
        &selector_for(&inventory, "oversized payload"),
    );
    assert_eq!(run.status, Some(101), "{}", run.stderr);
    let payload = failure_payload(&run.channel);
    let expected = format!("{}{}", "x".repeat(4096), " \u{2026}[truncated]");
    assert_eq!(
        payload.len(),
        expected.len(),
        "the rendering is cut at the budget, then the marker is appended"
    );
    assert_eq!(payload, expected);
}

/// A succeeding `?` is ordinary: the body runs on and completes (spec 6.7:14).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_succeeding_test_body_question_completes_normally() {
    let (image, inventory) = try_fixture();
    let run = run_test_image(&image, "try-ok", &selector_for(&inventory, "succeeds"));
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "five\n");
    assert_eq!(run.channel, COMPLETE_FRAME);
}

/// The accepted consequence, observed rather than asserted away: the failing
/// path traps, so a live local's destructor does not run (spec 6.7:16).
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_failing_question_skips_a_live_local_destructor() {
    let (image, inventory) = try_fixture();
    let run = run_test_image(
        &image,
        "try-drop",
        &selector_for(&inventory, "skips destructors"),
    );
    assert_eq!(run.status, Some(101), "{}", run.stderr);
    assert_eq!(
        run.stdout, "",
        "a `drop fn` whose observable work is a write must not be observed on \
         the failing path"
    );
}
