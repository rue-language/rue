//! Identity of the compiler image that is currently executing.
//!
//! This is deliberately a driver concern. A daemon client and its service can
//! compare the values without making a pathname, version string, or caller
//! assertion part of the identity contract.

use std::fmt;

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
    scheme: RunningImageIdentityScheme,
    value: Vec<u8>,
}

impl RunningImageIdentity {
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
        Ok(Self { scheme, value })
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
    use std::path::Path;

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

    #[cfg(test)]
    pub(super) fn identity_from_path(
        path: &Path,
    ) -> Result<RunningImageIdentity, RunningImageIdentityError> {
        let file = File::open(path)
            .map_err(|error| unavailable_io("opening private test image", error))?;
        identity_from_open_file(file)
    }

    #[cfg(test)]
    pub(super) fn identity_from_file(
        file: File,
    ) -> Result<RunningImageIdentity, RunningImageIdentityError> {
        identity_from_open_file(file)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{RunningImageIdentity, RunningImageIdentityError, RunningImageIdentityScheme};
    use std::ffi::c_void;

    type CFDataRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type SecCodeRef = *const c_void;
    type OSStatus = i32;

    const ERR_SEC_SUCCESS: OSStatus = 0;
    const K_SEC_CS_DEFAULT_FLAGS: u32 = 0;
    const K_SEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;

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
        fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> *const c_void;
        fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        fn CFDataGetLength(data: CFDataRef) -> isize;
        fn CFRelease(value: *const c_void);
        static kSecCodeInfoUnique: CFStringRef;
    }

    pub(super) fn capture() -> Result<RunningImageIdentity, RunningImageIdentityError> {
        let mut code: SecCodeRef = std::ptr::null();
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

        let validity =
            unsafe { SecCodeCheckValidity(code, K_SEC_CS_DEFAULT_FLAGS, std::ptr::null()) };
        if validity != ERR_SEC_SUCCESS {
            unsafe { CFRelease(code) };
            return Err(unavailable_status("SecCodeCheckValidity", validity));
        }

        let mut information: CFDictionaryRef = std::ptr::null();
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

        let unique =
            unsafe { CFDictionaryGetValue(information, kSecCodeInfoUnique.cast()) as CFDataRef };
        let value = if unique.is_null() {
            None
        } else {
            let length = unsafe { CFDataGetLength(unique) };
            let pointer = unsafe { CFDataGetBytePtr(unique) };
            if length <= 0 || pointer.is_null() {
                None
            } else {
                // Copy while both the dictionary and the SecCode object that
                // supplied it are still owned. The public identity outlives
                // both native objects.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::fs::File;
    #[cfg(target_os = "linux")]
    use std::fs::OpenOptions;
    #[cfg(target_os = "linux")]
    use std::io::Write;
    #[cfg(target_os = "linux")]
    use std::sync::{Arc, Barrier};

    #[test]
    fn running_image_identity_is_native_or_explicitly_unavailable() {
        match running_image_identity() {
            Ok(identity) => assert!(!identity.as_bytes().is_empty()),
            Err(error) => {
                assert!(error.reason().contains("unavailable") || !error.reason().is_empty())
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn open_handle_keeps_the_old_identity_across_same_path_replacement() {
        let directory = tempfile::tempdir().expect("private identity test directory");
        let image = directory.path().join("compiler");
        let replacement = directory.path().join("compiler.replacement");
        let current = std::env::current_exe().expect("current test executable");
        fs::copy(&current, &image).expect("copy private compiler image");
        fs::copy(&current, &replacement).expect("copy private replacement image");
        OpenOptions::new()
            .append(true)
            .open(&replacement)
            .expect("open private replacement")
            .write_all(b"replacement")
            .expect("make the replacement image distinct");

        let opened = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let opened_thread = Arc::clone(&opened);
        let release_thread = Arc::clone(&release);
        let old_file = File::open(&image).expect("open private image");
        let old_identity = std::thread::spawn(move || {
            opened_thread.wait();
            release_thread.wait();
            super::linux::identity_from_file(old_file).expect("identity from open image")
        });

        opened.wait();
        fs::rename(&replacement, &image).expect("replace private image path");
        release.wait();

        let captured_old = old_identity.join().expect("identity thread");
        let captured_new = super::linux::identity_from_path(&image).expect("new image identity");
        assert_ne!(captured_old, captured_new);
    }
}
