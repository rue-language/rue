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
use rue_error::{PreviewFeature, PreviewFeatures};

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
        preview_features: PreviewFeatures::from([PreviewFeature::TestDeclarations]),
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

/// A trapping body takes the ordinary runtime abort path and writes no frame.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_a_trapping_test_exits_101_without_a_frame() {
    let (image, _) = dispatch_fixture();
    let run = run_test_image(&image, "trap", "0000000000000002");
    assert_eq!(run.status, Some(101));
    assert_eq!(run.stderr, "panic: boom\n");
    assert_eq!(run.channel, "");
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
