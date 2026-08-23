use rue_compiler::unstable::{CancellableCompileOutcome, CompilationCancellation, OneShotMetrics};
use rue_compiler::{CompileErrors, CompileOptions, CompileWarning};
use rue_driver::{FilesystemCompilerHost, WatchInput};

use crate::output::{
    PublicationDestination, PublishRequest, publish_executable, publish_watch_executable,
};

/// Typed ownership boundary for the compile/link/publish operation.
pub(crate) struct CompileRequest<'a> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) options: CompileOptions,
    pub(crate) destination: PublicationDestination,
}

pub(crate) struct CancellableCompileRequest<'a> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) options: CompileOptions,
    pub(crate) destination: PublicationDestination,
    pub(crate) cancellation: CompilationCancellation,
    pub(crate) watch_inputs: Vec<WatchInput>,
}

pub(crate) enum CompileCycleOutcome {
    Linked(Box<LinkedExecutable>),
    Errors(CompileErrors),
    Canceled,
}

pub(crate) struct LinkedExecutable {
    target: rue_target::Target,
    warnings: Vec<CompileWarning>,
    metrics: OneShotMetrics,
    linked_bytes: Vec<u8>,
    destination: PublicationDestination,
    observation: PublicationObservation,
}

enum PublicationObservation {
    OneShot,
    Watch(Vec<WatchInput>),
}

pub(crate) struct PublishedExecutable {
    metrics: OneShotMetrics,
}

pub(crate) struct PublicationAttempt {
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) result: Result<PublishedExecutable, crate::output::PublishError>,
}

/// Execute the canonical compiler session through linking.
pub(crate) fn execute(request: CompileRequest<'_>) -> Result<LinkedExecutable, CompileErrors> {
    let output = request.host.executable_in_compile_scope(&request.options)?;
    let metrics = output.unstable_metrics();
    Ok(LinkedExecutable {
        target: request.options.target,
        warnings: output.warnings,
        metrics,
        linked_bytes: output.elf,
        destination: request.destination,
        observation: PublicationObservation::OneShot,
    })
}

pub(crate) fn execute_cancellable(request: CancellableCompileRequest<'_>) -> CompileCycleOutcome {
    let output = request
        .host
        .cancellable_executable_in_compile_scope(&request.options, request.cancellation);
    match output {
        CancellableCompileOutcome::Completed(output) => {
            let output = *output;
            let metrics = output.unstable_metrics();
            CompileCycleOutcome::Linked(Box::new(LinkedExecutable {
                target: request.options.target,
                warnings: output.warnings,
                metrics,
                linked_bytes: output.elf,
                destination: request.destination,
                observation: PublicationObservation::Watch(request.watch_inputs),
            }))
        }
        CancellableCompileOutcome::Errors(errors) => CompileCycleOutcome::Errors(errors),
        CancellableCompileOutcome::Canceled => CompileCycleOutcome::Canceled,
    }
}

impl LinkedExecutable {
    /// Finalize and publish the linked bytes after compiler timing/accounting closes.
    pub(crate) fn publish(self) -> PublicationAttempt {
        let LinkedExecutable {
            target,
            warnings,
            metrics,
            linked_bytes,
            destination,
            observation,
        } = self;
        let publication = match observation {
            PublicationObservation::OneShot => publish_executable(PublishRequest {
                destination,
                bytes: &linked_bytes,
                target,
            }),
            PublicationObservation::Watch(inputs) => publish_watch_executable(
                PublishRequest {
                    destination,
                    bytes: &linked_bytes,
                    target,
                },
                &inputs,
            ),
        };
        PublicationAttempt {
            warnings,
            result: publication.map(|()| PublishedExecutable { metrics }),
        }
    }
}

impl PublishedExecutable {
    pub(crate) fn unstable_metrics(&self) -> OneShotMetrics {
        self.metrics
    }
}
