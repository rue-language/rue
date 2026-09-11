use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rue_driver::{WatchInput, watch_inputs_changed};
use rue_error::{CompileError, ErrorKind};
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
    display_path: PathBuf,
    source_paths: Vec<PathBuf>,
}

impl PublicationDestination {
    /// A destination the service's preflight already validated, carried to
    /// the client that publishes it (ADR-0085 §5). The publication guard
    /// revalidates it against the same source set before the rename.
    pub(crate) fn from_parts(
        path: PathBuf,
        display_path: PathBuf,
        source_paths: Vec<PathBuf>,
    ) -> Self {
        Self {
            path,
            display_path,
            source_paths,
        }
    }

    /// `(path, display_path, source_paths)`.
    pub(crate) fn into_parts(self) -> (PathBuf, PathBuf, Vec<PathBuf>) {
        (self.path, self.display_path, self.source_paths)
    }
}

#[derive(Debug)]
pub(crate) enum PublishError {
    /// The destination is, or became, one of the program's own input sources.
    /// The refused path travels with the variant so every driver renders one
    /// message naming the output the user asked for.
    WouldClobberSource {
        path: PathBuf,
    },
    InputsChanged,
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

    /// Finalizers operate on anchored paths. Project their structured path
    /// fields to the invocation spelling without changing tool stderr or the
    /// underlying I/O error.
    fn with_display_path(mut self, actual: &Path, display: &Path) -> Self {
        match &mut self {
            Self::WouldClobberSource { path } | Self::Signing { path, .. } => {
                if path == actual {
                    *path = display.to_owned();
                }
            }
            Self::Io { paths, .. } => {
                for path in paths {
                    if path == actual {
                        *path = display.to_owned();
                    }
                }
            }
            Self::InputsChanged => {}
        }
        self
    }

    pub(crate) fn into_compile_error(self) -> CompileError {
        let message = match self {
            Self::WouldClobberSource { path } => format!(
                "output path '{}' is also an input source file; refusing to overwrite it",
                path.display()
            ),
            Self::InputsChanged => "an accepted input changed before output publication".to_owned(),
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
    if destination
        .source_paths
        .iter()
        .any(|source| output_would_clobber(&output_key, output_metadata.as_ref(), source))
    {
        return Err(PublishError::WouldClobberSource {
            path: destination.display_path.clone(),
        });
    }
    Ok(())
}

/// Validate source/output identity before compilation and retain the complete
/// source set for mandatory revalidation immediately before publication.
#[cfg(test)]
pub(crate) fn preflight_destination<'a>(
    path: &Path,
    source_paths: impl IntoIterator<Item = &'a str>,
) -> Result<PublicationDestination, PublishError> {
    preflight_destination_with_display(path, path, source_paths)
}

pub(crate) fn preflight_destination_with_display<'a>(
    path: &Path,
    display_path: &Path,
    source_paths: impl IntoIterator<Item = &'a str>,
) -> Result<PublicationDestination, PublishError> {
    preflight_destination_paths_with_display(
        path,
        display_path,
        source_paths.into_iter().map(PathBuf::from),
    )
}

fn preflight_destination_paths_with_display(
    path: &Path,
    display_path: &Path,
    source_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<PublicationDestination, PublishError> {
    let destination = PublicationDestination {
        path: path.to_owned(),
        display_path: display_path.to_owned(),
        source_paths: source_paths.into_iter().collect(),
    };
    validate_destination(&destination)?;
    Ok(destination)
}

#[cfg(test)]
pub(crate) fn preflight_watch_destination(
    path: &Path,
    inputs: &[WatchInput],
) -> Result<PublicationDestination, PublishError> {
    preflight_watch_destination_with_display(path, path, inputs)
}

pub(crate) fn preflight_watch_destination_with_display(
    path: &Path,
    display_path: &Path,
    inputs: &[WatchInput],
) -> Result<PublicationDestination, PublishError> {
    let source_paths = inputs
        .iter()
        .flat_map(|input| [input.requested_path(), input.canonical_path()])
        .map(Path::to_owned)
        .collect::<Vec<_>>();
    preflight_destination_paths_with_display(path, display_path, source_paths)
}

struct PendingOutput {
    path: PathBuf,
    display_path: PathBuf,
}

impl PendingOutput {
    fn publish(mut self) {
        self.path = PathBuf::new();
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_pending_output(
    destination: &PublicationDestination,
) -> Result<(PendingOutput, fs::File), PublishError> {
    let directory = destination.path.parent().unwrap_or_else(|| Path::new("."));
    let display_directory = destination
        .display_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rue-output");
    for attempt in 0..1000_u32 {
        let filename = format!(".{name}.rue-tmp-{}-{attempt}", std::process::id());
        let path = directory.join(&filename);
        let display_path = display_directory.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((PendingOutput { path, display_path }, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(PublishError::io(
                    "could not create temporary executable",
                    vec![display_path],
                    error,
                ));
            }
        }
    }
    Err(PublishError::io(
        "could not allocate a temporary executable path",
        vec![destination.display_path.clone()],
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

/// Publish a watch candidate only if the accepted source observations still
/// hold at the final publication boundary.
pub(crate) fn publish_watch_executable(
    request: PublishRequest<'_>,
    inputs: &[WatchInput],
) -> Result<(), PublishError> {
    publish_executable_with_finalizer_and_observation(
        request,
        Some(inputs),
        finalize_executable,
        || {},
    )
}

fn publish_executable_with_finalizer(
    request: PublishRequest<'_>,
    finalizer: impl FnOnce(&Path, Target) -> Result<(), PublishError>,
) -> Result<(), PublishError> {
    publish_executable_with_finalizer_and_observation(request, None, finalizer, || {})
}

fn publish_executable_with_finalizer_and_observation(
    request: PublishRequest<'_>,
    watch_inputs: Option<&[WatchInput]>,
    finalizer: impl FnOnce(&Path, Target) -> Result<(), PublishError>,
    before_rename: impl FnOnce(),
) -> Result<(), PublishError> {
    validate_destination(&request.destination)?;
    let destination = &request.destination.path;
    let (pending, mut file) = create_pending_output(&request.destination)?;
    file.write_all(request.bytes).map_err(|error| {
        PublishError::io(
            "could not write temporary executable",
            vec![pending.display_path.clone()],
            error,
        )
    })?;
    file.flush().map_err(|error| {
        PublishError::io(
            "could not flush temporary executable",
            vec![pending.display_path.clone()],
            error,
        )
    })?;
    drop(file);

    finalizer(&pending.path, request.target)
        .map_err(|error| error.with_display_path(&pending.path, &pending.display_path))?;

    // The hook is used only by deterministic unit tests to mutate an input in
    // the narrow window this check protects: after finalization/signing and
    // before the atomic replacement.
    before_rename();
    // The destination may have become an alias of an input while the
    // candidate was being finalized. Check it at the same boundary as the
    // accepted-input observations, before the atomic replacement.
    let destination_error = validate_destination(&request.destination).err();
    let all_inputs_changed = watch_inputs.is_some_and(watch_inputs_changed);
    if let Some(error) = destination_error {
        return Err(error);
    }
    if all_inputs_changed {
        return Err(PublishError::InputsChanged);
    }

    replace_destination(&pending.path, destination).map_err(|error| {
        PublishError::io(
            "could not atomically install finished executable",
            vec![
                pending.display_path.clone(),
                request.destination.display_path.clone(),
            ],
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

    #[test]
    fn anchored_creation_errors_keep_the_requested_output_directory() {
        let directory = temporary_directory("publish-display-create");
        let display = Path::new("missing/program");
        let destination = preflight_destination_with_display(
            &directory.join(display),
            display,
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let error = publish_executable(PublishRequest {
            destination,
            bytes: b"new",
            target: Target::X86_64Linux,
        })
        .unwrap_err();
        let PublishError::Io {
            operation, paths, ..
        } = error
        else {
            panic!("a missing parent must report the creation failure");
        };
        assert_eq!(operation, "could not create temporary executable");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].parent(), Some(Path::new("missing")));
        assert!(
            paths[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".program.rue-tmp-")
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn anchored_rename_errors_keep_both_requested_path_spellings() {
        let directory = temporary_directory("publish-display-rename");
        fs::create_dir(directory.join("program")).unwrap();
        let destination = preflight_destination_with_display(
            &directory.join("program"),
            Path::new("program"),
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let error = publish_executable(PublishRequest {
            destination,
            bytes: b"new",
            target: Target::X86_64Linux,
        })
        .unwrap_err();
        let PublishError::Io {
            operation, paths, ..
        } = error
        else {
            panic!("replacing a directory must report the rename failure");
        };
        assert_eq!(
            operation,
            "could not atomically install finished executable"
        );
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].parent(), Some(Path::new("")));
        assert_eq!(paths[1], Path::new("program"));
        assert!(directory.join("program").is_dir());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn anchored_finalizer_errors_project_paths_and_preserve_tool_errors() {
        let directory = temporary_directory("publish-display-finalize");
        let destination_path = directory.join("program");
        fs::write(&destination_path, b"old").unwrap();
        let tool_message = format!("opaque tool detail naming {}", directory.display());
        for signing in [false, true] {
            let destination = preflight_destination_with_display(
                &destination_path,
                Path::new("program"),
                std::iter::empty::<&str>(),
            )
            .unwrap();
            let error = publish_executable_with_finalizer(
                PublishRequest {
                    destination,
                    bytes: b"new",
                    target: Target::X86_64Linux,
                },
                |temporary, _target| {
                    assert!(temporary.is_absolute());
                    assert_eq!(fs::read(temporary).unwrap(), b"new");
                    if signing {
                        Err(PublishError::Signing {
                            path: temporary.to_owned(),
                            error: SigningError::Rejected(tool_message.clone()),
                        })
                    } else {
                        Err(PublishError::io(
                            "injected finalizer I/O failure",
                            vec![temporary.to_owned()],
                            io::Error::other(tool_message.clone()),
                        ))
                    }
                },
            )
            .unwrap_err();
            let display = match error {
                PublishError::Signing {
                    path,
                    error: SigningError::Rejected(stderr),
                } => {
                    assert!(signing);
                    assert_eq!(stderr, tool_message);
                    path
                }
                PublishError::Io {
                    mut paths, error, ..
                } => {
                    assert!(!signing);
                    assert_eq!(error.to_string(), tool_message);
                    assert_eq!(paths.len(), 1);
                    paths.remove(0)
                }
                _ => panic!("the finalizer's typed error must survive publication"),
            };
            assert_eq!(display.parent(), Some(Path::new("")));
            assert_eq!(fs::read(&destination_path).unwrap(), b"old");
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        }
        fs::remove_dir_all(directory).unwrap();
    }

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
    fn changed_accepted_input_at_final_boundary_preserves_destination() {
        let directory = temporary_directory("publish-input-change");
        let source = directory.join("main.rue");
        let destination = directory.join("program");
        fs::write(&source, b"old source").unwrap();
        fs::write(&destination, b"old executable").unwrap();
        let publication_destination =
            preflight_destination(&destination, [source.to_str().unwrap()]).unwrap();
        let watch_inputs = vec![WatchInput::new(
            source.clone(),
            source.clone(),
            rue_driver::WatchFingerprint::from_bytes(b"old source"),
        )];

        let result = publish_executable_with_finalizer_and_observation(
            PublishRequest {
                destination: publication_destination,
                bytes: b"new executable",
                target: Target::X86_64Linux,
            },
            Some(&watch_inputs),
            |temporary, _target| {
                fs::metadata(temporary).unwrap();
                Ok(())
            },
            || fs::write(&source, b"new source").unwrap(),
        );

        assert!(matches!(result, Err(PublishError::InputsChanged)));
        assert_eq!(fs::read(&destination).unwrap(), b"old executable");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".program.rue-tmp-")
        }));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn watch_preflight_refuses_an_expected_absence_output_collision() {
        let directory = temporary_directory("watch-absence-clobber");
        let destination = directory.join("candidate.rue");
        let inputs = vec![WatchInput::expected_absence(destination.clone())];

        assert!(matches!(
            preflight_watch_destination(&destination, &inputs),
            Err(PublishError::WouldClobberSource { .. })
        ));
        assert!(!destination.exists());
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
            Err(PublishError::WouldClobberSource { .. })
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
