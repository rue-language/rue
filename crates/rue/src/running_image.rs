//! Identity of the compiler image that is currently executing.
//!
//! This is deliberately a driver concern. A daemon client and its service can
//! compare the values without making a pathname, version string, or caller
//! assertion part of the identity contract.

use std::fmt;

/// Version of the public running-image identity domain.
pub const RUNNING_IMAGE_IDENTITY_SCHEME_VERSION: u16 = 1;

/// Host CPU architecture carried by a running-image identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RunningImageArchitecture {
    X86_64,
    Aarch64,
    Other(String),
}

impl RunningImageArchitecture {
    fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            architecture => Self::Other(architecture.to_owned()),
        }
    }
}

/// The native mechanism used to obtain a [`RunningImageIdentity`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RunningImageIdentityScheme {
    /// SHA-256 of the image read through an open `/proc/self/exe` handle.
    LinuxProcSelfExeSha256,
    /// The owned `kSecCodeInfoUnique` value for the dynamically validated
    /// `SecCodeCopySelf` object.
    MacOsSecCodeInfoUnique,
}

/// An owned identity for the exact compiler image that is running.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RunningImageIdentity {
    scheme_version: u16,
    architecture: RunningImageArchitecture,
    scheme: RunningImageIdentityScheme,
    value: Vec<u8>,
}

impl RunningImageIdentity {
    /// Returns the version of the identity scheme domain.
    pub fn scheme_version(&self) -> u16 {
        self.scheme_version
    }

    /// Returns the host architecture that produced this identity.
    pub fn architecture(&self) -> &RunningImageArchitecture {
        &self.architecture
    }

    /// Returns the native identity scheme, which is part of the comparison
    /// domain and is not inferred from the caller's platform claim.
    pub fn scheme(&self) -> RunningImageIdentityScheme {
        self.scheme
    }

    /// Returns the owned identity bytes in their native representation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.value
    }

    fn new(
        scheme: RunningImageIdentityScheme,
        value: Vec<u8>,
    ) -> Result<Self, RunningImageIdentityError> {
        if value.is_empty() {
            return Err(RunningImageIdentityError::unavailable(
                "the native identity was empty",
            ));
        }
        Ok(Self {
            scheme_version: RUNNING_IMAGE_IDENTITY_SCHEME_VERSION,
            architecture: RunningImageArchitecture::current(),
            scheme,
            value,
        })
    }
}

/// Why the current image identity could not be established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningImageIdentityError {
    reason: String,
}

impl RunningImageIdentityError {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Returns a stable, user-facing explanation for the unavailable result.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for RunningImageIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "running compiler image identity unavailable: {}",
            self.reason
        )
    }
}

impl std::error::Error for RunningImageIdentityError {}

/// Capture the identity of the image containing the current process.
///
/// There is no pathname or metadata fallback. Callers that require a daemon
/// identity must handle `Err` as an unavailable identity and decline to use
/// the service.
pub fn running_image_identity() -> Result<RunningImageIdentity, RunningImageIdentityError> {
    #[cfg(target_os = "linux")]
    {
        return linux::capture();
    }

    #[cfg(target_os = "macos")]
    {
        return macos::capture();
    }

    #[allow(unreachable_code)]
    Err(RunningImageIdentityError::unavailable(
        "this host platform has no native running-image identity implementation",
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{RunningImageIdentity, RunningImageIdentityError, RunningImageIdentityScheme};
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use std::io::{self, Read};

    pub(super) fn capture() -> Result<RunningImageIdentity, RunningImageIdentityError> {
        let file = File::open("/proc/self/exe")
            .map_err(|error| unavailable_io("opening /proc/self/exe", error))?;
        identity_from_open_file(file)
    }

    fn identity_from_open_file(
        mut file: File,
    ) -> Result<RunningImageIdentity, RunningImageIdentityError> {
        let metadata = file
            .metadata()
            .map_err(|error| unavailable_io("reading /proc/self/exe metadata", error))?;
        if !metadata.file_type().is_file() {
            return Err(RunningImageIdentityError::unavailable(
                "/proc/self/exe did not resolve to a regular file",
            ));
        }

        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| unavailable_io("reading /proc/self/exe", error))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }

        RunningImageIdentity::new(
            RunningImageIdentityScheme::LinuxProcSelfExeSha256,
            hasher.finalize().to_vec(),
        )
    }

    fn unavailable_io(operation: &str, error: io::Error) -> RunningImageIdentityError {
        RunningImageIdentityError::unavailable(format!("{operation}: {error}"))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{RunningImageIdentity, RunningImageIdentityError, RunningImageIdentityScheme};
    use std::ffi::c_void;

    type CFDataRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFTypeRef = *const c_void;
    type SecCodeRef = *const c_void;
    type OSStatus = i32;

    const ERR_SEC_SUCCESS: OSStatus = 0;
    // kSecCSDefaultFlags. kSecCSUseAllArchitectures (1 << 0) is accepted by
    // static-code lookup, not by SecCodeCheckValidity; passing it to the
    // latter returns errSecCSInvalidFlags (-67070).
    const K_SEC_CS_DEFAULT_FLAGS: u32 = 0;
    // kSecCSSigningInformation from SecCode.h.
    const K_SEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;

    #[cfg(test)]
    const TEST_STAGE_CONTROL_ENV: &str = "RUE_RUNNING_IMAGE_TEST_STAGE_CONTROL";

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecCodeCopySelf(flags: u32, self_code: *mut SecCodeRef) -> OSStatus;
        fn SecCodeCheckValidity(
            code: SecCodeRef,
            flags: u32,
            requirement: *const c_void,
        ) -> OSStatus;
        fn SecCodeCopySigningInformation(
            code: SecCodeRef,
            flags: u32,
            information: *mut CFDictionaryRef,
        ) -> OSStatus;
        fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> CFTypeRef;
        fn CFGetTypeID(value: CFTypeRef) -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        fn CFDataGetLength(data: CFDataRef) -> isize;
        fn CFRelease(value: *const c_void);
        static kSecCodeInfoUnique: CFStringRef;
    }

    pub(super) fn capture() -> Result<RunningImageIdentity, RunningImageIdentityError> {
        let mut code: SecCodeRef = std::ptr::null();
        // SAFETY: The output is initialized by Security.framework. A
        // successful SecCodeCopySelf returns one owned SecCode reference,
        // released exactly once below; the defensive error-path release
        // handles a non-null output accompanying an error status.
        let status = unsafe { SecCodeCopySelf(K_SEC_CS_DEFAULT_FLAGS, &mut code) };
        if status != ERR_SEC_SUCCESS {
            unsafe {
                if !code.is_null() {
                    CFRelease(code);
                }
            }
            return Err(unavailable_status("SecCodeCopySelf", status));
        }
        if code.is_null() {
            return Err(RunningImageIdentityError::unavailable(
                "SecCodeCopySelf returned no code object",
            ));
        }
        #[cfg(test)]
        test_stage_barrier("copy-self");

        // SAFETY: `code` is the live, owned SecCode object returned above.
        // SecCodeCheckValidity borrows it and uses no requirement object.
        let validity =
            unsafe { SecCodeCheckValidity(code, K_SEC_CS_DEFAULT_FLAGS, std::ptr::null()) };
        if validity != ERR_SEC_SUCCESS {
            unsafe { CFRelease(code) };
            return Err(unavailable_status("SecCodeCheckValidity", validity));
        }
        #[cfg(test)]
        test_stage_barrier("check-validity");

        let mut information: CFDictionaryRef = std::ptr::null();
        // SAFETY: The output dictionary is a new +1 CoreFoundation object on
        // success. It is released exactly once after the unique value has
        // been copied, and `code` remains owned until that point.
        let signing_information = unsafe {
            SecCodeCopySigningInformation(code, K_SEC_CS_SIGNING_INFORMATION, &mut information)
        };
        if signing_information != ERR_SEC_SUCCESS || information.is_null() {
            unsafe {
                if !information.is_null() {
                    CFRelease(information);
                }
                CFRelease(code);
            }
            return Err(unavailable_status(
                "SecCodeCopySigningInformation",
                signing_information,
            ));
        }
        #[cfg(test)]
        test_stage_barrier("signing-information");

        let unique = unsafe { CFDictionaryGetValue(information, kSecCodeInfoUnique.cast()) };
        let value = if unique.is_null() {
            None
        } else if unsafe { CFGetTypeID(unique) != CFDataGetTypeID() } {
            None
        } else {
            let data = unique as CFDataRef;
            let length = unsafe { CFDataGetLength(data) };
            let pointer = unsafe { CFDataGetBytePtr(data) };
            if length <= 0 || pointer.is_null() {
                None
            } else {
                // Copy while both the dictionary and the SecCode object that
                // supplied it are still owned. The public identity outlives
                // both native objects, and the type-ID check above prevents
                // treating an unrelated CF object as CFData.
                Some(unsafe { std::slice::from_raw_parts(pointer, length as usize).to_vec() })
            }
        };

        unsafe {
            CFRelease(information);
            CFRelease(code);
        }

        let Some(value) = value else {
            return Err(RunningImageIdentityError::unavailable(
                "the validated SecCode object had no owned kSecCodeInfoUnique value",
            ));
        };
        RunningImageIdentity::new(RunningImageIdentityScheme::MacOsSecCodeInfoUnique, value)
    }

    fn unavailable_status(operation: &str, status: OSStatus) -> RunningImageIdentityError {
        RunningImageIdentityError::unavailable(format!("{operation} failed with OSStatus {status}"))
    }

    #[cfg(test)]
    fn test_stage_barrier(stage: &str) {
        let Some(control) = std::env::var_os(TEST_STAGE_CONTROL_ENV) else {
            return;
        };
        let control = std::path::PathBuf::from(control);
        std::fs::write(control.join(format!("stage-{stage}-ready")), b"ready")
            .expect("write native identity stage barrier");
        let release = control.join(format!("stage-{stage}-release"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !release.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for native identity stage {stage}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const HELPER_CONTROL_ENV: &str = "RUE_RUNNING_IMAGE_HELPER_CONTROL";
    const HELPER_ONCE_ENV: &str = "RUE_RUNNING_IMAGE_HELPER_ONCE";
    const HELPER_STAGED_ENV: &str = "RUE_RUNNING_IMAGE_HELPER_STAGED";
    #[cfg(target_os = "macos")]
    const NATIVE_STAGE_CONTROL_ENV: &str = "RUE_RUNNING_IMAGE_TEST_STAGE_CONTROL";
    const FIXTURE_VERSION: &str = "running-image-fixture-v1";
    const HELPER_WAIT: Duration = Duration::from_secs(20);

    #[test]
    fn running_image_identity_has_an_explicit_comparison_domain() {
        if let Ok(identity) = running_image_identity() {
            assert_eq!(
                identity.scheme_version(),
                RUNNING_IMAGE_IDENTITY_SCHEME_VERSION
            );
            assert!(!identity.as_bytes().is_empty());
            assert!(matches!(
                identity.architecture(),
                RunningImageArchitecture::X86_64 | RunningImageArchitecture::Aarch64
            ));
        }
    }

    #[test]
    fn subprocess_identity_is_bound_to_the_running_image_not_its_path() {
        let directory = tempfile::tempdir().expect("private identity test directory");
        let image_a = directory.path().join("compiler-a");
        let image_b = directory.path().join("compiler-b");
        let image_b_probe = directory.path().join("compiler-b-probe");
        let current = std::env::current_exe().expect("current test executable");
        fs::copy(&current, &image_a).expect("copy private image A");
        fs::copy(&current, &image_b).expect("copy private image B");
        fs::copy(&current, &image_b_probe).expect("copy private probe image B");
        #[cfg(target_os = "macos")]
        {
            // Strip the copied test binary's possibly stale signature before
            // changing bytes. Removing it after mutation is rejected by
            // codesign because the old CodeDirectory is already corrupt.
            remove_signature(&image_a);
            remove_signature(&image_b);
            remove_signature(&image_b_probe);
        }
        append_fixture_difference(&image_b);
        #[cfg(target_os = "macos")]
        {
            sign_adhoc(&image_a, "running-image-fixture-a");
            sign_adhoc(&image_b, "running-image-fixture-b");
            assert_eq!(macho_uuid(&image_a), macho_uuid(&image_b));
        }
        fs::copy(&image_b, &image_b_probe).expect("copy the signed replacement fixture");

        let bytes_a = fs::read(&image_a).expect("read fixture image A");
        let bytes_b = fs::read(&image_b).expect("read fixture image B");
        let bytes_b_probe = fs::read(&image_b_probe).expect("read replacement probe image B");
        assert_ne!(bytes_a, bytes_b, "the fixtures must have different content");
        assert_eq!(
            bytes_b, bytes_b_probe,
            "B and its probe must be byte-identical"
        );

        // Establish that a properly signed private replacement can execute
        // and produce an identity before using it in the replacement race.
        let replacement_control = tempfile::tempdir().expect("replacement control directory");
        let mut replacement = spawn_helper(&image_b_probe, replacement_control.path(), true, false);
        wait_for_marker(replacement.child_mut(), replacement_control.path(), "done");
        let replacement_identity = read_capture(replacement_control.path(), "once");
        replacement.wait_success();
        assert_ok(
            &replacement_identity,
            "signed/private image B must capture successfully",
        );
        #[cfg(target_os = "macos")]
        macos_stage_barrier_fixture(&image_a);

        let old_control = tempfile::tempdir().expect("old-process control directory");
        let mut old = spawn_helper(&image_a, old_control.path(), false, false);
        wait_for_marker(old.child_mut(), old_control.path(), "ready");
        let before = read_capture(old_control.path(), "before");
        assert_ok(&before, "signed/private image A must capture successfully");

        fs::remove_file(&image_a).expect("delete old image path before replacement");
        fs::rename(&image_b, &image_a).expect("replace image A at the same path");
        touch(old_control.path().join("between"));
        let between_alive =
            wait_for_marker_or_terminated(old.child_mut(), old_control.path(), "between-ready");
        let between = if between_alive {
            let capture = read_capture(old_control.path(), "between");
            // Capture the post-replacement state while the replacement path
            // still exists. macOS may terminate an old unlinked executable
            // during the next dynamic validation; that is handled below as
            // an explicit unavailable result.
            touch(old_control.path().join("after"));
            if wait_for_marker_or_terminated(old.child_mut(), old_control.path(), "done") {
                let after = read_capture(old_control.path(), "after");
                old.wait_success();
                assert_old_image_or_unavailable(&before, &after, "after replacement");
            } else {
                let after = format!(
                    "{FIXTURE_VERSION}\nerr\nplatform terminated old image during replacement\n"
                );
                assert_old_image_or_unavailable(&before, &after, "after replacement");
            }
            capture
        } else {
            format!("{FIXTURE_VERSION}\nerr\nplatform terminated old image during replacement\n")
        };
        assert_old_image_or_unavailable(&before, &between, "between replacement");
        fs::remove_file(&image_a).expect("delete the replacement path");

        assert_ne!(identity_key(&before), identity_key(&replacement_identity));
        assert_eq!(fixture_version(&before), FIXTURE_VERSION);
        assert_eq!(fixture_version(&replacement_identity), FIXTURE_VERSION);

        #[cfg(target_os = "macos")]
        macos_negative_fixtures(directory.path(), &current);
    }

    #[test]
    fn subprocess_identity_helper() {
        let Some(control) = std::env::var_os(HELPER_CONTROL_ENV) else {
            return;
        };
        let control = PathBuf::from(control);
        fs::write(control.join("version"), FIXTURE_VERSION).expect("write fixture version");

        if std::env::var_os(HELPER_ONCE_ENV).is_some() {
            write_capture(&control, "once");
            touch(control.join("done"));
            return;
        }

        write_capture(&control, "before");
        touch(control.join("ready"));
        wait_for_parent_marker(&control, "between");
        write_capture(&control, "between");
        touch(control.join("between-ready"));
        wait_for_parent_marker(&control, "after");
        write_capture(&control, "after");
        touch(control.join("done"));
    }

    fn append_fixture_difference(path: &Path) {
        #[cfg(target_os = "macos")]
        {
            // Appending bytes after a Mach-O image fails strict codesign
            // validation. Adding an unused loader path changes the image's
            // load commands while keeping it executable and re-signable.
            let output = Command::new("/usr/bin/install_name_tool")
                .args(["-add_rpath", "@loader_path"])
                .arg(path)
                .output()
                .expect("modify private Mach-O fixture");
            assert!(
                output.status.success(),
                "install_name_tool failed for {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        #[cfg(not(target_os = "macos"))]
        {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(path)
                .expect("open fixture for content difference")
                .write_all(b"different-image-content")
                .expect("append fixture content difference");
        }
    }

    fn spawn_helper(path: &Path, control: &Path, once: bool, staged: bool) -> HelperChild {
        let mut command = Command::new(path);
        command
            .args([
                "--exact",
                "running_image::tests::subprocess_identity_helper",
                "--nocapture",
            ])
            .env(HELPER_CONTROL_ENV, control)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if once {
            command.env(HELPER_ONCE_ENV, "1");
        }
        if staged {
            command.env(HELPER_STAGED_ENV, "1");
            #[cfg(target_os = "macos")]
            command.env(NATIVE_STAGE_CONTROL_ENV, control);
        }
        HelperChild {
            child: command.spawn().expect("spawn private identity fixture"),
        }
    }

    struct HelperChild {
        child: Child,
    }

    impl HelperChild {
        fn child_mut(&mut self) -> &mut Child {
            &mut self.child
        }

        fn wait_success(&mut self) {
            let status = self.child.wait().expect("wait for identity fixture");
            assert!(status.success(), "identity fixture exited with {status}");
        }
    }

    impl Drop for HelperChild {
        fn drop(&mut self) {
            if self
                .child
                .try_wait()
                .expect("poll identity fixture")
                .is_none()
            {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    fn wait_for_marker(child: &mut Child, directory: &Path, marker: &str) {
        let deadline = Instant::now() + HELPER_WAIT;
        let path = directory.join(marker);
        loop {
            if path.exists() {
                return;
            }
            if let Some(status) = child.try_wait().expect("poll identity fixture") {
                panic!("identity fixture exited before {marker}: {status}");
            }
            assert!(Instant::now() < deadline, "timed out waiting for {marker}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_marker_or_terminated(child: &mut Child, directory: &Path, marker: &str) -> bool {
        let deadline = Instant::now() + HELPER_WAIT;
        let path = directory.join(marker);
        loop {
            if path.exists() {
                return true;
            }
            if let Some(status) = child.try_wait().expect("poll identity fixture") {
                assert!(
                    !status.success(),
                    "identity fixture exited cleanly before {marker}"
                );
                return false;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {marker}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_parent_marker(directory: &Path, marker: &str) {
        let deadline = Instant::now() + HELPER_WAIT;
        let path = directory.join(marker);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for parent {marker}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn touch(path: PathBuf) {
        fs::write(path, b"ready").expect("write identity fixture barrier");
    }

    fn write_capture(directory: &Path, name: &str) {
        let result = match running_image_identity() {
            Ok(identity) => format!(
                "{FIXTURE_VERSION}\nok\n{}\n{:?}\n{:?}\n{}\n",
                identity.scheme_version(),
                identity.architecture(),
                identity.scheme(),
                hex(identity.as_bytes()),
            ),
            Err(error) => format!("{FIXTURE_VERSION}\nerr\n{}\n", error.reason()),
        };
        fs::write(directory.join(name), result).expect("write identity capture");
    }

    fn read_capture(directory: &Path, name: &str) -> String {
        fs::read_to_string(directory.join(name)).expect("read identity capture")
    }

    fn assert_ok(capture: &str, message: &str) {
        assert_eq!(capture.lines().nth(1), Some("ok"), "{message}: {capture}");
    }

    fn assert_old_image_or_unavailable(before: &str, later: &str, phase: &str) {
        if later.lines().nth(1) == Some("ok") {
            assert_eq!(
                identity_key(before),
                identity_key(later),
                "old image changed {phase}"
            );
        } else {
            assert_eq!(
                later.lines().nth(1),
                Some("err"),
                "invalid result {phase}: {later}"
            );
        }
    }

    fn identity_key(capture: &str) -> Option<String> {
        let lines: Vec<_> = capture.lines().collect();
        (lines.get(1) == Some(&"ok")).then(|| lines[1..=5].join("\n"))
    }

    fn fixture_version(capture: &str) -> &str {
        capture.lines().next().expect("fixture version line")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[cfg(target_os = "macos")]
    fn macos_stage_barrier_fixture(path: &Path) {
        let control = tempfile::tempdir().expect("native identity stage control directory");
        let mut child = spawn_helper(path, control.path(), true, true);
        for stage in ["copy-self", "check-validity", "signing-information"] {
            let ready = format!("stage-{stage}-ready");
            wait_for_marker(child.child_mut(), control.path(), &ready);
            touch(control.path().join(format!("stage-{stage}-release")));
        }
        wait_for_marker(child.child_mut(), control.path(), "done");
        let capture = read_capture(control.path(), "once");
        child.wait_success();
        assert_ok(
            &capture,
            "staged signed/private image must capture successfully",
        );
    }

    #[cfg(target_os = "macos")]
    fn sign_adhoc(path: &Path, identifier: &str) {
        let output = Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--sign",
                "-",
                "--identifier",
                identifier,
                "--timestamp=none",
            ])
            .arg(path)
            .output()
            .expect("run codesign for private identity fixture");
        assert!(
            output.status.success(),
            "ad-hoc signing failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    fn remove_signature(path: &Path) {
        let output = Command::new("/usr/bin/codesign")
            .args(["--remove-signature"])
            .arg(path)
            .output()
            .expect("remove existing signature from private identity fixture");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() || error.contains("code object is not signed"),
            "removing existing signature failed for {}: {}",
            path.display(),
            error
        );
    }

    #[cfg(target_os = "macos")]
    fn macho_uuid(path: &Path) -> String {
        let output = Command::new("/usr/bin/otool")
            .args(["-l"])
            .arg(path)
            .output()
            .expect("inspect private Mach-O UUID");
        assert!(
            output.status.success(),
            "otool failed for {}",
            path.display()
        );
        let mut saw_uuid_command = false;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if line.trim() == "cmd LC_UUID" {
                saw_uuid_command = true;
                continue;
            }
            if saw_uuid_command && line.trim_start().starts_with("uuid ") {
                return line.trim_start()[5..].to_owned();
            }
        }
        panic!("{} has no LC_UUID", path.display());
    }

    #[cfg(target_os = "macos")]
    fn macos_negative_fixtures(directory: &Path, current: &Path) {
        let unsigned = directory.join("compiler-unsigned");
        fs::copy(current, &unsigned).expect("copy unsigned fixture");
        remove_signature(&unsigned);
        let control = tempfile::tempdir().expect("unsigned fixture control directory");
        let mut child = spawn_helper(&unsigned, control.path(), true, false);
        if wait_for_marker_or_terminated(child.child_mut(), control.path(), "done") {
            let capture = read_capture(control.path(), "once");
            child.wait_success();
            assert_eq!(
                capture.lines().nth(1),
                Some("err"),
                "unsigned fixture validated"
            );
        }
    }
}
