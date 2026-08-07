//! Retained target-object projections of canonical codegen terminals.
//!
//! Object serialization is a typed query downstream of one exact
//! `CodegenUnit` key. The fresh linker remains deliberately stateless: it
//! consumes these stable bytes on every link, while unchanged units reuse the
//! retained projection owned by the compiler query graph.

use std::{hash::Hash, sync::Arc};

use rue_query::{
    QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutcome, QueryOutput, QueryTerminalKind,
};

use crate::retained_charge::RetainedCharge;

/// Object-container format selected unambiguously by the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ObjectFormat {
    Elf,
    MachO,
}

impl ObjectFormat {
    pub(crate) fn for_target(target: rue_target::Target) -> Self {
        if target.is_macho() {
            Self::MachO
        } else {
            Self::Elf
        }
    }
}

/// Exact identity of one target-object projection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectProjectionQueryKey {
    /// The dependency key names the exact canonical CodegenUnit terminal. Its
    /// backend and ABI-layout epochs remain part of this query's identity.
    pub(crate) codegen: crate::codegen_query::CodegenUnitQueryKey,
    pub(crate) target: rue_target::Target,
    pub(crate) object_format: ObjectFormat,
    pub(crate) abi_layout_epoch: u32,
    pub(crate) object_writer_epoch: u32,
}

impl ObjectProjectionQueryKey {
    pub(crate) fn new(codegen: crate::codegen_query::CodegenUnitQueryKey) -> Self {
        let target = codegen.target;
        Self {
            abi_layout_epoch: codegen.abi_layout_epoch,
            codegen,
            target,
            object_format: ObjectFormat::for_target(target),
            object_writer_epoch: OBJECT_WRITER_EPOCH,
        }
    }
}

impl QueryKey for ObjectProjectionQueryKey {
    fn stable_identity(&self) -> String {
        format!(
            "object-projection;{};target={:?};format={:?};abi-layout={};writer={}",
            self.codegen.stable_identity(),
            self.target,
            self.object_format,
            self.abi_layout_epoch,
            self.object_writer_epoch,
        )
    }
}

/// Retained object-container bytes for one codegen unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectProjection {
    pub(crate) bytes: Arc<[u8]>,
}

impl RetainedCharge for ObjectProjection {
    fn retained_charge(&self) -> u64 {
        self.bytes.retained_charge()
    }
}

/// Typed object projection terminal. Serialization failures remain ordinary
/// compiler diagnostics in the canonical query graph.
#[derive(Debug, Clone)]
pub(crate) enum ObjectProjectionValue {
    Available(Arc<ObjectProjection>),
    Failure(crate::CompileErrors),
}

impl RetainedCharge for ObjectProjectionValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(object) => object.retained_charge(),
            Self::Failure(errors) => errors.retained_charge(),
        }
    }
}

pub(crate) fn object_projection_value_equal(
    left: &ObjectProjectionValue,
    right: &ObjectProjectionValue,
) -> bool {
    match (left, right) {
        (ObjectProjectionValue::Available(left), ObjectProjectionValue::Available(right)) => {
            left == right
        }
        (ObjectProjectionValue::Failure(left), ObjectProjectionValue::Failure(right)) => {
            left == right
        }
        _ => false,
    }
}

/// Stable production collection retaining both the codegen metadata needed by
/// `ProgramImagePlan` and its already-serialized object bytes.
#[derive(Clone)]
pub(crate) struct CollectedObjectProjection {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) unit: Arc<crate::codegen_query::CodegenUnit>,
    pub(crate) object: Arc<ObjectProjection>,
}

pub(crate) fn evaluate_object_projection(
    context: &QueryContext,
    codegen_units: &QueryFamily<
        crate::codegen_query::CodegenUnitQueryKey,
        crate::codegen_query::CodegenUnitValue,
    >,
    key: &ObjectProjectionQueryKey,
) -> Result<QueryOutput<ObjectProjectionValue>, QueryAbort> {
    context.check_canceled()?;
    let codegen = context.query_registered(codegen_units, key.codegen.clone())?;
    let QueryOutcome::Success(codegen) = codegen.outcome() else {
        unreachable!("CodegenUnit publishes typed values")
    };
    let crate::codegen_query::CodegenUnitValue::Available(unit) = codegen else {
        let crate::codegen_query::CodegenUnitValue::Failure(errors) = codegen else {
            unreachable!()
        };
        return Ok(object_failure(errors.clone()));
    };
    context.record_work(rue_query::WorkItem::new("object.projection.attempts", 1));
    let value = match crate::backend::project_backend_object(unit.backend_product(), key.target) {
        Ok(bytes) => ObjectProjectionValue::Available(Arc::new(ObjectProjection {
            bytes: bytes.into(),
        })),
        Err(error) => return Ok(object_failure(error.into())),
    };
    Ok(QueryOutput::success(value))
}

fn object_failure(errors: crate::CompileErrors) -> QueryOutput<ObjectProjectionValue> {
    QueryOutput::success(ObjectProjectionValue::Failure(errors))
        .with_terminal_kind(QueryTerminalKind::Failure)
}

// Bump whenever object-container serialization changes independently of
// CodegenUnit's backend/ABI-layout epochs.
const OBJECT_WRITER_EPOCH: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct FailureKey;

    impl QueryKey for FailureKey {
        fn stable_identity(&self) -> String {
            "object-failure".to_owned()
        }
    }

    #[test]
    fn retained_charge_accounts_object_bytes() {
        let object = ObjectProjection {
            bytes: Arc::from([1_u8, 2, 3, 4]),
        };
        assert_eq!(object.retained_charge(), 4);
        assert_eq!(
            ObjectProjectionValue::Available(Arc::new(object)).retained_charge(),
            std::mem::size_of::<ObjectProjection>() as u64 + 4
        );
    }

    #[test]
    fn typed_object_failure_is_a_failure_terminal() {
        let runtime = rue_query::QueryRuntime::new(1);
        let revision = rue_query::Revision::new(1, 1);
        runtime.publish_revision(revision, []).unwrap();
        let family = runtime
            .family_with_equality_and_evaluator(
                "test.object-projection-failure",
                1,
                object_projection_value_equal,
                |_, _, _| {
                    Ok(object_failure(
                        crate::CompileError::without_span(crate::ErrorKind::InternalCodegenError(
                            "object failure".to_owned(),
                        ))
                        .into(),
                    ))
                },
            )
            .unwrap();
        let terminal = runtime
            .request_registered(
                &family,
                revision,
                FailureKey,
                rue_query::CancellationToken::new(),
            )
            .into_result()
            .unwrap();
        assert_eq!(terminal.kind(), QueryTerminalKind::Failure);
        assert!(matches!(
            terminal.outcome(),
            QueryOutcome::Success(ObjectProjectionValue::Failure(_))
        ));
    }
}
