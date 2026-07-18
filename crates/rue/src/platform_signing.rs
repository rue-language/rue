use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use rue_target::Target;

pub(crate) struct SigningRequest<'a> {
    pub(crate) path: &'a Path,
    pub(crate) target: Target,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SigningResult {
    NotRequired,
    Signed,
}

#[derive(Debug)]
pub(crate) enum SigningError {
    Launch(io::Error),
    Rejected(String),
}

impl std::fmt::Display for SigningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(error) => write!(formatter, "could not run codesign: {error}"),
            Self::Rejected(stderr) => write!(formatter, "codesign failed: {stderr}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SigningCommand {
    program: &'static OsStr,
    arguments: Vec<OsString>,
}

#[derive(Debug)]
struct CommandResult {
    success: bool,
    stderr: Vec<u8>,
}

fn signing_command(request: &SigningRequest<'_>, host_is_macos: bool) -> Option<SigningCommand> {
    if !host_is_macos || !request.target.is_macho() {
        return None;
    }
    Some(SigningCommand {
        program: OsStr::new("codesign"),
        arguments: [
            OsString::from("-f"),
            OsString::from("-s"),
            OsString::from("-"),
            OsString::from("--identifier"),
            OsString::from("dev.rue-lang.program"),
            OsString::from("--timestamp=none"),
            request.path.as_os_str().to_owned(),
        ]
        .into(),
    })
}

fn sign_with_runner(
    request: SigningRequest<'_>,
    host_is_macos: bool,
    runner: impl FnOnce(&SigningCommand) -> io::Result<CommandResult>,
) -> Result<SigningResult, SigningError> {
    let Some(command) = signing_command(&request, host_is_macos) else {
        return Ok(SigningResult::NotRequired);
    };
    match runner(&command) {
        Ok(output) if output.success => Ok(SigningResult::Signed),
        Ok(output) => Err(SigningError::Rejected(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )),
        Err(error) => Err(SigningError::Launch(error)),
    }
}

/// Apply required platform signing before an executable is made visible.
pub(crate) fn sign_executable(request: SigningRequest<'_>) -> Result<SigningResult, SigningError> {
    sign_with_runner(request, cfg!(target_os = "macos"), |command| {
        let output = std::process::Command::new(command.program)
            .args(&command.arguments)
            .output()?;
        Ok(CommandResult {
            success: output.status.success(),
            stderr: output.stderr,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn macos_signing_command_pins_identity_timestamp_and_path() {
        let path = PathBuf::from("/tmp/program-under-publication");
        let request = SigningRequest {
            path: &path,
            target: Target::Aarch64Macos,
        };
        let command = signing_command(&request, true).unwrap();
        assert_eq!(command.program, OsStr::new("codesign"));
        assert_eq!(
            command.arguments,
            [
                "-f",
                "-s",
                "-",
                "--identifier",
                "dev.rue-lang.program",
                "--timestamp=none",
                "/tmp/program-under-publication",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn signing_policy_skips_non_macos_targets_and_hosts() {
        let path = PathBuf::from("program");
        assert!(
            signing_command(
                &SigningRequest {
                    path: &path,
                    target: Target::X86_64Linux,
                },
                true
            )
            .is_none()
        );
        assert!(
            signing_command(
                &SigningRequest {
                    path: &path,
                    target: Target::Aarch64Macos,
                },
                false
            )
            .is_none()
        );
    }

    #[test]
    fn signing_failures_are_fatal_typed_errors() {
        let path = PathBuf::from("program");
        let request = SigningRequest {
            path: &path,
            target: Target::Aarch64Macos,
        };
        let failure = sign_with_runner(request, true, |_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "missing codesign"))
        });
        assert!(
            matches!(failure, Err(SigningError::Launch(error)) if error.kind() == io::ErrorKind::NotFound)
        );

        let rejected = sign_with_runner(
            SigningRequest {
                path: &path,
                target: Target::Aarch64Macos,
            },
            true,
            |_| {
                Ok(CommandResult {
                    success: false,
                    stderr: b"bad signature".to_vec(),
                })
            },
        );
        assert!(
            matches!(rejected, Err(SigningError::Rejected(message)) if message == "bad signature")
        );
    }
}
