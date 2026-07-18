use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rue_compiler::{CompileError, ErrorKind};
use rue_target::Target;

use crate::platform_signing::{SigningError, SigningRequest, sign_executable};

/// Complete request for publishing a linked executable.
pub(crate) struct PublishRequest<'a> {
    pub(crate) destination: PublicationDestination,
    pub(crate) bytes: &'a [u8],
    pub(crate) target: Target,
}

pub(crate) struct PublicationDestination {
    path: PathBuf,
    source_paths: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum PublishError {
    WouldClobberSource,
    Io {
        operation: &'static str,
        paths: Vec<PathBuf>,
        error: io::Error,
    },
    Signing {
        path: PathBuf,
        error: SigningError,
    },
}

impl PublishError {
    fn io(operation: &'static str, paths: Vec<PathBuf>, error: io::Error) -> Self {
        Self::Io {
            operation,
            paths,
            error,
        }
    }

    pub(crate) fn into_compile_error(self) -> CompileError {
        let message = match self {
            Self::WouldClobberSource => {
                "output path is also an input source file; refusing to overwrite it".to_owned()
            }
            Self::Io {
                operation,
                paths,
                error,
            } => format!(
                "{operation} ({}): {error}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::Signing { path, error } => {
                format!(
                    "could not sign temporary executable {}: {error}",
                    path.display()
                )
            }
        };
        CompileError::without_span(ErrorKind::OutputPublication(message))
    }
}

fn clobber_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            fs::canonicalize(parent)
                .map(|canonical| canonical.join(name))
                .unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn output_would_clobber(
    output_key: &Path,
    output_metadata: Option<&fs::Metadata>,
    source: &Path,
) -> bool {
    if clobber_key(source) == output_key {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Some(output), Ok(source)) = (output_metadata, fs::metadata(source)) {
            return output.dev() == source.dev() && output.ino() == source.ino();
        }
    }
    false
}

fn validate_destination(destination: &PublicationDestination) -> Result<(), PublishError> {
    let output_key = clobber_key(&destination.path);
    let output_metadata = fs::metadata(&destination.path).ok();
    if destination.source_paths.iter().any(|source| {
        output_would_clobber(&output_key, output_metadata.as_ref(), Path::new(source))
    }) {
        return Err(PublishError::WouldClobberSource);
    }
    Ok(())
}

/// Validate source/output identity before compilation and retain the complete
/// source set for mandatory revalidation immediately before publication.
pub(crate) fn preflight_destination<'a>(
    path: &Path,
    source_paths: impl IntoIterator<Item = &'a str>,
) -> Result<PublicationDestination, PublishError> {
    let destination = PublicationDestination {
        path: path.to_owned(),
        source_paths: source_paths.into_iter().map(str::to_owned).collect(),
    };
    validate_destination(&destination)?;
    Ok(destination)
}

struct PendingOutput(PathBuf);

impl PendingOutput {
    fn publish(mut self) {
        self.0 = PathBuf::new();
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn create_pending_output(destination: &Path) -> Result<(PendingOutput, fs::File), PublishError> {
    let directory = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rue-output");
    for attempt in 0..1000_u32 {
        let path = directory.join(format!(".{name}.rue-tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((PendingOutput(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(PublishError::io(
                    "could not create temporary executable",
                    vec![path],
                    error,
                ));
            }
        }
    }
    Err(PublishError::io(
        "could not allocate a temporary executable path",
        vec![destination.to_owned()],
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "all temporary output names already exist",
        ),
    ))
}

fn finalize_executable(path: &Path, target: Target) -> Result<(), PublishError> {
    #[cfg(unix)]
    {
        let metadata = fs::metadata(path).map_err(|error| {
            PublishError::io(
                "could not read temporary executable metadata",
                vec![path.to_owned()],
                error,
            )
        })?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|error| {
            PublishError::io(
                "could not set temporary executable permissions",
                vec![path.to_owned()],
                error,
            )
        })?;
    }

    sign_executable(SigningRequest { path, target }).map_err(|error| PublishError::Signing {
        path: path.to_owned(),
        error,
    })?;
    Ok(())
}

/// Write, finalize, and atomically publish an executable in its destination directory.
/// Every failure retains its operation and affected path context.
///
/// Until the final rename succeeds, an existing destination is untouched. The
/// cleanup guard removes partial output on every failure path.
pub(crate) fn publish_executable(request: PublishRequest<'_>) -> Result<(), PublishError> {
    publish_executable_with_finalizer(request, finalize_executable)
}

fn publish_executable_with_finalizer(
    request: PublishRequest<'_>,
    finalizer: impl FnOnce(&Path, Target) -> Result<(), PublishError>,
) -> Result<(), PublishError> {
    validate_destination(&request.destination)?;
    let destination = &request.destination.path;
    let (pending, mut file) = create_pending_output(destination)?;
    file.write_all(request.bytes).map_err(|error| {
        PublishError::io(
            "could not write temporary executable",
            vec![pending.0.clone()],
            error,
        )
    })?;
    file.flush().map_err(|error| {
        PublishError::io(
            "could not flush temporary executable",
            vec![pending.0.clone()],
            error,
        )
    })?;
    drop(file);

    finalizer(&pending.0, request.target)?;

    replace_destination(&pending.0, destination).map_err(|error| {
        PublishError::io(
            "could not atomically install finished executable",
            vec![pending.0.clone(), destination.to_owned()],
            error,
        )
    })?;
    pending.publish();
    Ok(())
}

/// Supported Rue hosts are Unix systems, where rename replaces an existing
/// non-directory destination atomically. Other hosts refuse replacement rather
/// than opening a remove-then-rename window that could lose an old executable.
#[cfg(unix)]
fn replace_destination(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(unix))]
fn replace_destination(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic replacement of an existing output is unsupported on this host",
        ));
    }
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rue-{name}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn publication_replaces_the_destination_and_leaves_no_temporary_file() {
        let directory = temporary_directory("publish");
        let destination = directory.join("program");
        fs::write(&destination, b"old").unwrap();
        let destination = preflight_destination(&destination, std::iter::empty::<&str>()).unwrap();

        publish_executable(PublishRequest {
            destination,
            bytes: b"new executable",
            target: Target::X86_64Linux,
        })
        .unwrap();

        assert_eq!(
            fs::read(directory.join("program")).unwrap(),
            b"new executable"
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(directory.join("program"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_publication_preserves_an_existing_destination() {
        let directory = temporary_directory("publish-failure");
        let destination = directory.join("program");
        fs::create_dir(&destination).unwrap();
        let publication_destination =
            preflight_destination(&destination, std::iter::empty::<&str>()).unwrap();

        assert!(
            publish_executable(PublishRequest {
                destination: publication_destination,
                bytes: b"new",
                target: Target::X86_64Linux,
            })
            .is_err()
        );
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn finalization_failure_preserves_destination_and_removes_temporary_file() {
        let directory = temporary_directory("publish-finalization-failure");
        let destination = directory.join("program");
        fs::write(&destination, b"old executable").unwrap();
        let publication_destination =
            preflight_destination(&destination, std::iter::empty::<&str>()).unwrap();

        let result = publish_executable_with_finalizer(
            PublishRequest {
                destination: publication_destination,
                bytes: b"new executable",
                target: Target::X86_64Linux,
            },
            |temporary, _target| {
                Err(PublishError::io(
                    "injected finalization failure",
                    vec![temporary.to_owned()],
                    io::Error::other("finalizer rejected executable"),
                ))
            },
        );

        assert!(matches!(
            result,
            Err(PublishError::Io {
                operation: "injected finalization failure",
                ..
            })
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"old executable");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn publication_refuses_a_hard_link_to_any_source() {
        let directory = temporary_directory("publish-clobber");
        let source = directory.join("main.rue");
        let destination = directory.join("program");
        fs::write(&source, "fn main() -> i32 { 0 }\n").unwrap();
        let source_paths = vec![source.display().to_string()];
        let publication_destination =
            preflight_destination(&destination, source_paths.iter().map(String::as_str)).unwrap();
        // The filesystem can change between preflight and publication. Create
        // the alias afterwards to prove mandatory revalidation closes the gap.
        fs::hard_link(&source, &destination).unwrap();

        assert!(matches!(
            publish_executable(PublishRequest {
                destination: publication_destination,
                bytes: b"executable",
                target: Target::X86_64Linux,
            }),
            Err(PublishError::WouldClobberSource)
        ));
        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            "fn main() -> i32 { 0 }\n"
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(not(unix))]
    #[test]
    fn replacing_an_existing_destination_is_explicitly_refused() {
        let directory = temporary_directory("publish-replace-policy");
        let destination = directory.join("program");
        fs::write(&destination, b"old").unwrap();
        let destination = preflight_destination(&destination, std::iter::empty::<&str>()).unwrap();

        assert!(matches!(
            publish_executable(PublishRequest {
                destination,
                bytes: b"new",
                target: Target::X86_64Linux,
            }),
            Err(PublishError::Io { error, .. })
                if error.kind() == io::ErrorKind::Unsupported
        ));
        assert_eq!(fs::read(directory.join("program")).unwrap(), b"old");
        fs::remove_dir_all(directory).unwrap();
    }
}
