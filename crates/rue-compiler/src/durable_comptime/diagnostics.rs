//! Durable failure taxonomy and diagnostic mapping.
//!
//! This module maps already-established semantic, provider, and query
//! terminals. It owns no evaluator invocation or query orchestration.

use super::lifecycle::*;
use super::projection::*;
use super::*;

/// A revision-independent diagnostic location owned by the declaration that
/// produced a durable comptime fact.  Durable failures must carry this
/// semantic location rather than borrowing a caller span (or an instruction
/// reference) from the revision that happened to observe them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeDiagnosticSite {
    producer: DeclarationCandidateKey,
    start: u32,
    end: u32,
}

impl DurableComptimeDiagnosticSite {
    pub(crate) fn new(producer: DeclarationCandidateKey, start: u32, end: u32) -> Self {
        Self {
            producer,
            start,
            end,
        }
    }

    pub(super) fn into_parts(self) -> (DeclarationCandidateKey, u32, u32) {
        (self.producer, self.start, self.end)
    }

    #[cfg(test)]
    pub(super) fn producer_for_test(&self) -> &DeclarationCandidateKey {
        &self.producer
    }

    #[cfg(test)]
    pub(super) fn range_for_test(&self) -> (u32, u32) {
        (self.start, self.end)
    }
}

/// The durable boundary distinguishes query control from a committed
/// semantic failure.  In particular, cancellation and missing inputs remain
/// aborts and are never converted into diagnostics or memoized failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeFailure {
    Abort(QueryAbort),
    Failure(Box<SemanticNucleusFailure>),
}

/// The AIR error channel has one failure type for both tagged branches.  This
/// opaque payload keeps query aborts and semantic failures distinct; the
/// canonical constructors below always pair each payload with its matching
/// AIR outer tag.  AIR's public error enum still permits arbitrary payloads in
/// principle, so this guarantee is enforced by this durable funnel rather
/// than claimed as a property of the AIR type itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeHostFailure(DurableComptimeHostFailureKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableComptimeHostFailureKind {
    Semantic(Box<SemanticNucleusFailure>),
    QueryFailure(rue_query::QueryFailure),
    QueryAbort(QueryAbort),
}

impl DurableComptimeHostFailure {
    pub(super) fn semantic(failure: Box<SemanticNucleusFailure>) -> Self {
        Self(DurableComptimeHostFailureKind::Semantic(failure))
    }

    pub(super) fn query_abort(abort: QueryAbort) -> Self {
        Self(DurableComptimeHostFailureKind::QueryAbort(abort))
    }

    pub(super) fn query_failure(failure: rue_query::QueryFailure) -> Self {
        Self(DurableComptimeHostFailureKind::QueryFailure(failure))
    }

    pub(super) fn into_host_error(self) -> rue_air::ComptimeHostError<Self> {
        match &self.0 {
            DurableComptimeHostFailureKind::Semantic(_) => {
                rue_air::ComptimeHostError::HostFailure(self)
            }
            DurableComptimeHostFailureKind::QueryFailure(_) => {
                rue_air::ComptimeHostError::HostFailure(self)
            }
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                rue_air::ComptimeHostError::Abort(self)
            }
        }
    }

    /// Split the host's terminal channel at a declaration query root. AIR
    /// keeps semantic failures and retained query failures in the same
    /// `HostFailure` variant, while the query family must publish those two
    /// outcomes through different outer channels.
    pub(crate) fn into_root_host_failure(
        self,
    ) -> Result<Box<SemanticNucleusFailure>, rue_query::QueryFailure> {
        match self.0 {
            DurableComptimeHostFailureKind::Semantic(failure) => Ok(failure),
            DurableComptimeHostFailureKind::QueryFailure(failure) => Err(failure),
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                unreachable!("query aborts use the AIR Abort outcome")
            }
        }
    }

    pub(crate) fn into_root_abort(self) -> QueryAbort {
        match self.0 {
            DurableComptimeHostFailureKind::QueryAbort(abort) => abort,
            DurableComptimeHostFailureKind::Semantic(_)
            | DurableComptimeHostFailureKind::QueryFailure(_) => {
                unreachable!("semantic and retained query failures use HostFailure")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn semantic_failure(&self) -> Option<&SemanticNucleusFailure> {
        match &self.0 {
            DurableComptimeHostFailureKind::Semantic(failure) => Some(failure),
            DurableComptimeHostFailureKind::QueryFailure(_)
            | DurableComptimeHostFailureKind::QueryAbort(_) => None,
        }
    }
}

impl DurableComptimeFailure {
    pub(crate) fn failure(failure: SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }

    pub(crate) fn abort(abort: QueryAbort) -> Self {
        Self::Abort(abort)
    }

    pub(crate) fn resolution(message: impl Into<Arc<str>>) -> Self {
        Self::failure(SemanticNucleusFailure::Resolution(message.into()))
    }

    pub(crate) fn comptime_failure(reason: impl Into<String>) -> Self {
        Self::failure(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::ComptimeEvaluationFailed {
                reason: reason.into(),
            },
        ))
    }

    /// The exact durable terminal used when a comptime match reaches no
    /// selected arm. This remains a resolution failure, matching the
    /// established declaration-time behavior.
    /// evaluator's existing `comptime match has no selected arm` policy.
    pub(crate) fn comptime_match_no_selected_arm() -> Self {
        Self::resolution("comptime match has no selected arm")
    }

    /// Map the AIR-owned semantic rejection vocabulary to the exact durable
    /// declaration-time diagnostics. Ordinary AIR hosts intentionally keep
    /// these same reasons runtime-dependent.
    pub(crate) fn comptime_rejection(
        rejection: ComptimeSemanticRejection<EvaluatedSemanticConst>,
    ) -> Self {
        match rejection {
            ComptimeSemanticRejection::ConditionNotBoolean(value) => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => {
                    Self::resolution("comptime condition is not boolean")
                }
            },
            ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                operation: _operation,
                lhs,
                rhs,
            } => {
                let values = [Some(lhs.clone()), rhs.clone()];
                let target_count = values
                    .iter()
                    .flatten()
                    .filter(|value| matches!(value, EvaluatedSemanticConst::TargetEnum(_)))
                    .count();
                if rhs.is_some() && target_count == 2 {
                    return Self::resolution(
                        "target descriptors support only equality comparisons",
                    );
                }
                if rhs.is_some() && target_count == 1 {
                    return Self::resolution(
                        "target descriptor comparison requires matching enum variants",
                    );
                }
                for value in [Some(lhs), rhs.clone()].into_iter().flatten() {
                    match value {
                        EvaluatedSemanticConst::Module(_) => {
                            return Self::resolution("module used where a value is required");
                        }
                        EvaluatedSemanticConst::TargetEnum(_) => {
                            return Self::resolution(
                                "target descriptor used where a durable const value is required",
                            );
                        }
                        EvaluatedSemanticConst::Value(_) => {}
                    }
                }
                let bool_count = values
                    .iter()
                    .flatten()
                    .filter(|value| {
                        matches!(
                            value,
                            EvaluatedSemanticConst::Value(value)
                                if matches!(value.value, DurableConstValue::Bool(_))
                        )
                    })
                    .count();
                if rhs.is_some() && bool_count == 2 {
                    return Self::resolution("boolean values support only equality comparisons");
                }
                Self::resolution("comptime arithmetic operand is not an integer")
            }
            ComptimeSemanticRejection::UnaryOperandNotInteger(value) => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => {
                    Self::resolution("comptime arithmetic operand is not an integer")
                }
            },
            ComptimeSemanticRejection::UnaryTypeNotInteger { operation, value } => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => match operation {
                    ComptimeUnaryOperation::Neg => {
                        Self::resolution("comptime negation operand is not an integer")
                    }
                    ComptimeUnaryOperation::BitNot => {
                        Self::resolution("comptime bitwise NOT operand is not an integer")
                    }
                },
            },
            ComptimeSemanticRejection::Assignment => {
                Self::resolution("assignment is not supported in declaration-time comptime")
            }
            ComptimeSemanticRejection::AggregateExpression => Self::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: "aggregate expression".to_owned(),
                }),
            ),
            ComptimeSemanticRejection::EmptyBlock => {
                Self::resolution("comptime block has no result instruction")
            }
            ComptimeSemanticRejection::UnsupportedIntrinsic(name) => Self::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("intrinsic `@{name}`"),
                }),
            ),
            ComptimeSemanticRejection::FloatOperandWidthMismatch { .. } => {
                Self::resolution("comptime float operands have different types (f32 and f64)")
            }
            ComptimeSemanticRejection::FloatRemainder { .. } => {
                Self::resolution("`%` is not defined on floating-point operands; use std.math.rem")
            }
            ComptimeSemanticRejection::UnsupportedExpression => {
                Self::resolution("expression is not supported in declaration-time comptime")
            }
        }
    }

    pub(crate) fn maximum_depth(name: &str, maximum: usize) -> Self {
        Self::comptime_failure(format!(
            "specialization of '{name}' exceeded the maximum nesting depth ({maximum}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?"
        ))
    }

    pub(crate) fn integer_literal_overflow(type_name: &str, value: i128) -> Self {
        Self::comptime_failure(format!(
            "integer overflow evaluating constant at type {type_name}: value {value} is out of range for type {type_name}; {value} does not fit in {type_name} (this operation would panic at runtime)"
        ))
    }

    pub(crate) fn arithmetic_overflow(type_name: &str, operation: &str, detail: &str) -> Self {
        Self::comptime_failure(format!(
            "integer overflow evaluating {operation} at type {type_name}: {detail} (this operation would panic at runtime)"
        ))
    }

    /// Construct a diagnostic anchored to the owning declaration.  The site
    /// is explicit so nested/foreign evaluation cannot accidentally label the
    /// failure with the ambient caller's span.
    pub(crate) fn comptime_failure_at(
        site: &DurableComptimeDiagnosticSite,
        reason: impl Into<String>,
    ) -> Self {
        Self::diagnostic_at_site(site, reason.into())
    }

    fn diagnostic_at_site(site: &DurableComptimeDiagnosticSite, reason: String) -> Self {
        Self::failure(SemanticNucleusFailure::DiagnosticAtProducerRange {
            kind: rue_error::ErrorKind::ComptimeEvaluationFailed { reason },
            producer: site.producer.clone(),
            start: site.start,
            end: site.end,
        })
    }

    /// Adapt a supported AIR arithmetic trap using the owning declaration and
    /// the trap's own span.  Unsupported operations remain unsupported rather
    /// than acquiring a new diagnostic spelling.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn trap_at(
        producer: DeclarationCandidateKey,
        trap: rue_air::ComptimeTrap,
    ) -> Option<Self> {
        let site = DurableComptimeDiagnosticSite::new(producer, trap.span.start, trap.span.end);
        let reason = arithmetic_trap_reason(trap.operation)?;
        Some(Self::diagnostic_at_site(&site, reason.to_owned()))
    }

    /// Adapt a durable terminal directly for the AIR host.  Provider
    /// errors use `provider_error_as_host` instead, so this seam never creates
    /// a durable failure only to convert it back into a provider error.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn into_host_error(self) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        match self {
            Self::Failure(failure) => {
                DurableComptimeHostFailure::semantic(failure).into_host_error()
            }
            Self::Abort(abort) => DurableComptimeHostFailure::query_abort(abort).into_host_error(),
        }
    }

    /// Build the AIR error directly from a provider result.  This is the
    /// AIR host funnel; the query-root adapter below only unwraps its matching
    /// outer channel and never normalizes crossed tags.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn provider_error_as_host(
        error: rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    ) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        match error {
            rue_air::SemanticProviderError::Abort(abort) => {
                DurableComptimeHostFailure::query_abort(abort).into_host_error()
            }
            rue_air::SemanticProviderError::Failure(failure) => {
                DurableComptimeHostFailure::semantic(Box::new(failure)).into_host_error()
            }
        }
    }
}

fn arithmetic_trap_reason(operation: &str) -> Option<&'static str> {
    match operation {
        "division by zero" => Some("division by zero (this operation would panic at runtime)"),
        "remainder by zero" => Some("remainder by zero (this operation would panic at runtime)"),
        _ => None,
    }
}

pub(super) fn durable_host_failure(error: DurableComptimeFailure) -> DurableComptimeHostFailure {
    match error {
        DurableComptimeFailure::Failure(failure) => DurableComptimeHostFailure::semantic(failure),
        DurableComptimeFailure::Abort(abort) => DurableComptimeHostFailure::query_abort(abort),
    }
}

pub(super) fn durable_diagnostic_failure(
    _site: &DurableComptimeDiagnosticSite,
    kind: rue_error::ErrorKind,
) -> DurableComptimeHostFailure {
    DurableComptimeHostFailure::semantic(Box::new(SemanticNucleusFailure::Diagnostic(kind)))
}

/// A durable comptime diagnostic that carries a `help:` line.
///
/// The plain [`durable_diagnostic_failure`] has no help slot; the nucleus
/// failure it builds does, so a gate whose diagnostic is meaningless without
/// its remedy (the preview gates) uses this instead.
pub(super) fn durable_diagnostic_failure_with_help(
    _site: &DurableComptimeDiagnosticSite,
    kind: rue_error::ErrorKind,
    help: String,
) -> DurableComptimeHostFailure {
    DurableComptimeHostFailure::semantic(Box::new(SemanticNucleusFailure::DiagnosticWithHelp {
        kind,
        help: std::sync::Arc::from(help.as_str()),
    }))
}

pub(super) fn durable_host_error(
    error: DurableComptimeFailure,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    error.into_host_error()
}

pub(super) fn durable_provider_error(
    error: rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    match error {
        rue_air::SemanticProviderError::Abort(error) => {
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(error))
        }
        rue_air::SemanticProviderError::Failure(error) => rue_air::ComptimeHostError::HostFailure(
            DurableComptimeHostFailure::semantic(Box::new(error)),
        ),
    }
}

pub(super) fn durable_foreign_call_error(
    error: DurableComptimeForeignCallError,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    match error {
        DurableComptimeForeignCallError::ReadyFailure(failure) => {
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::semantic(Box::new(
                failure,
            )))
        }
        DurableComptimeForeignCallError::StructuredFrame(
            DurableComptimeStructuredFrameAdmissionError::ValueFit(failure),
        ) => rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::semantic(failure)),
        DurableComptimeForeignCallError::ReadyQueryFailure(failure) => {
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::query_failure(
                failure,
            ))
        }
        DurableComptimeForeignCallError::AdmissionFailure(failure) => {
            durable_projection_failure_error(failure)
        }
        DurableComptimeForeignCallError::FrameAdmission(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_frame_admission_failure_reason(failure)),
        ),
        DurableComptimeForeignCallError::StructuredFrame(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_structured_frame_failure_reason(failure)),
        ),
        DurableComptimeForeignCallError::UnexpectedReadyProjection => {
            durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call returned an unexpected projection",
            ))
        }
        DurableComptimeForeignCallError::Lifecycle(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_lifecycle_failure_reason(failure)),
        ),
    }
}

/// Maps candidate-artifact failures into the durable semantic channel.  Query
/// failures remain an outer query channel; callers must preserve that split.
pub(crate) fn durable_candidate_rir_semantic_failure(
    failure: &crate::revisioned_query_database::DeclarationBodyPlanFailure,
) -> SemanticNucleusFailure {
    use crate::revisioned_query_database::DeclarationBodyPlanFailure as Artifact;

    match failure {
        Artifact::Build(kind) => SemanticNucleusFailure::Diagnostic(kind.clone()),
        Artifact::CandidateRirRejected(errors) => {
            SemanticNucleusFailure::Syntax(Arc::from(errors.to_string()))
        }
        failure => SemanticNucleusFailure::Resolution(Arc::from(format!(
            "candidate RIR artifact failed: {failure:?}"
        ))),
    }
}

/// Maps candidate materialization failures into the durable semantic channel,
/// retaining query cancellation as an outer abort.
pub(crate) fn durable_materialization_semantic_failure(
    failure: crate::canonical_lower::BodyPlanMaterializationFailure,
) -> Result<SemanticNucleusFailure, QueryAbort> {
    use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;

    match failure {
        Materialization::Query(abort) => Err(abort),
        Materialization::Build(error) => Ok(SemanticNucleusFailure::Diagnostic(
            crate::canonical_lower::rir_build_error_kind(
                "declaration-time candidate materialization",
                &error,
            ),
        )),
        Materialization::Invalid(detail) => Ok(SemanticNucleusFailure::Resolution(detail)),
    }
}

pub(super) fn durable_projection_failure_error(
    failure: crate::body_query::ComptimeProgramProjectionFailure,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    use crate::body_query::ComptimeProgramProjectionFailure as Projection;
    use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;
    use SemanticNucleusFailure as Failure;

    let failure = match failure {
        Projection::Materialization(Materialization::Query(abort)) => {
            return rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(
                abort,
            ));
        }
        Projection::Materialization(failure) => {
            match durable_materialization_semantic_failure(failure) {
                Ok(failure) => failure,
                Err(abort) => {
                    return rue_air::ComptimeHostError::Abort(
                        DurableComptimeHostFailure::query_abort(abort),
                    );
                }
            }
        }
        Projection::Artifact(failure) => durable_candidate_rir_semantic_failure(&failure),
        Projection::ArtifactQueryFailure(failure) => {
            return rue_air::ComptimeHostError::HostFailure(
                DurableComptimeHostFailure::query_failure(failure),
            );
        }
        Projection::NotFunction { .. } => Failure::Resolution(Arc::from(
            "comptime candidate artifact has a non-function root",
        )),
        Projection::NotConst { .. } => Failure::Resolution(Arc::from(
            "constant candidate artifact has a non-constant root",
        )),
        Projection::InvalidProducer(producer) => Failure::Resolution(Arc::from(format!(
            "{:?}",
            Projection::InvalidProducer(producer)
        ))),
        Projection::IdentityMismatch => {
            Failure::Resolution(Arc::from(format!("{:?}", Projection::IdentityMismatch)))
        }
    };
    DurableComptimeHostFailure::semantic(Box::new(failure)).into_host_error()
}

pub(super) fn durable_frame_admission_failure_reason(
    failure: DurableComptimeForeignFrameAdmissionError,
) -> &'static str {
    match failure {
        DurableComptimeForeignFrameAdmissionError::NotCallable => {
            "durable comptime call root is not callable"
        }
        DurableComptimeForeignFrameAdmissionError::TicketMismatch => {
            "durable comptime call ticket does not match its program"
        }
        DurableComptimeForeignFrameAdmissionError::RegistryMismatch => {
            "durable comptime call program is not registered"
        }
    }
}

pub(super) fn durable_structured_frame_failure_reason(
    failure: DurableComptimeStructuredFrameAdmissionError,
) -> &'static str {
    match failure {
        DurableComptimeStructuredFrameAdmissionError::InvalidContract => {
            "durable structured comptime call has an invalid contract"
        }
        DurableComptimeStructuredFrameAdmissionError::ValueFit(_) => {
            "durable structured comptime call argument does not fit"
        }
        DurableComptimeStructuredFrameAdmissionError::ResultNotType => {
            "durable structured comptime call result is not a type"
        }
    }
}

pub(super) fn durable_lifecycle_failure_reason(
    failure: DurableComptimeLifecycleError,
) -> &'static str {
    match failure {
        DurableComptimeLifecycleError::TicketMismatch => "durable call lifecycle ticket mismatch",
        DurableComptimeLifecycleError::BindingMismatch => "durable call lifecycle binding mismatch",
        DurableComptimeLifecycleError::InvalidProgramAuthority => {
            "durable call lifecycle invalid program authority"
        }
        DurableComptimeLifecycleError::NotEntered => "durable call lifecycle call was not entered",
        DurableComptimeLifecycleError::OutOfOrder => {
            "durable call lifecycle call finished out of order"
        }
        DurableComptimeLifecycleError::TicketReused => "durable call lifecycle ticket was reused",
        DurableComptimeLifecycleError::InvalidContext => "durable call lifecycle invalid context",
        DurableComptimeLifecycleError::ReadyProjectionRequired => {
            "durable call lifecycle requires a ready projection"
        }
    }
}

/// Normalized type-syntax terminal shared by the durable comptime and
/// signature-query adapters. Nested comptime-call arguments are reduced to
/// their terminal so each context can apply its own diagnostic policy once.
#[derive(Debug)]
pub(crate) enum DurableTypeSyntaxClassification {
    Abort(QueryAbort),
    Failure(SemanticNucleusFailure),
    Semantic(rue_air::SemanticTypeSyntaxFailure<crate::StableDefinitionKey, Arc<str>>),
}

pub(crate) fn classify_durable_type_syntax_failure(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> DurableTypeSyntaxClassification {
    use rue_air::SemanticResolutionError as E;

    match error {
        E::ProviderAbort(abort) => DurableTypeSyntaxClassification::Abort(abort),
        E::ProviderFailure(failure) => DurableTypeSyntaxClassification::Failure(failure),
        E::Semantic(failure) => DurableTypeSyntaxClassification::Semantic(failure),
        E::ComptimeCallTypeArgument { error, .. } => classify_durable_type_syntax_failure(*error),
    }
}

/// Durable comptime's type-syntax adapter preserves its historical
/// resolution-shaped debug diagnostic. Signature queries intentionally use a
/// separate detailed adapter in `revisioned_query_database`.
pub(crate) fn durable_comptime_type_syntax_failure(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> DurableComptimeFailure {
    match classify_durable_type_syntax_failure(error) {
        DurableTypeSyntaxClassification::Abort(abort) => DurableComptimeFailure::abort(abort),
        DurableTypeSyntaxClassification::Failure(failure) => {
            DurableComptimeFailure::failure(failure)
        }
        DurableTypeSyntaxClassification::Semantic(failure) => {
            DurableComptimeFailure::resolution(format!("Semantic({failure:?})"))
        }
    }
}

pub(super) fn durable_type_syntax_error(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    durable_comptime_type_syntax_failure(error).into_host_error()
}

pub(super) fn durable_host_error_outcome<T>(
    error: rue_air::ComptimeHostError<DurableComptimeHostFailure>,
) -> rue_air::ComptimeOutcome<T, DurableComptimeHostFailure> {
    match error {
        rue_air::ComptimeHostError::HostFailure(error) => {
            rue_air::ComptimeOutcome::HostFailure(error)
        }
        rue_air::ComptimeHostError::Abort(error) => rue_air::ComptimeOutcome::Abort(error),
    }
}

/// AIR supplies compact operator tokens; durable diagnostics use the
/// operation names from established declaration-time semantics. Unary negation is
/// passed as its own token by the canonical engine so it cannot be confused
/// with subtraction.
pub(super) fn durable_arithmetic_operation_name(operation: &str) -> &str {
    match operation {
        "+" => "addition",
        "-" => "subtraction",
        "*" => "multiplication",
        "/" => "division",
        "%" => "remainder",
        "<<" => "left shift",
        ">>" => "right shift",
        "negation" => "negation",
        other => other,
    }
}

#[cfg(test)]
mod terminal_adapter_tests {
    use super::*;
    use rue_air::ComptimeIntegerOperation;

    fn site(name: &str, start: u32, end: u32) -> DurableComptimeDiagnosticSite {
        DurableComptimeDiagnosticSite::new(
            DeclarationCandidateKey {
                module: ModuleId::from_validated_canonical("terminal-tests"),
                category:
                    crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
                name: Arc::from(name),
                owner: None,
                duplicate_discriminator: 0,
            },
            start,
            end,
        )
    }

    #[test]
    fn durable_failure_preserves_query_control_and_domain_channels() {
        let aborts = [
            QueryAbort::Canceled,
            QueryAbort::Cycle(Arc::from([])),
            QueryAbort::ForeignRuntime,
            QueryAbort::UnpublishedRevision(rue_query::Revision::new(7, 8)),
            QueryAbort::MissingInput(rue_query::InputIdentity::new("terminal", "missing")),
        ];
        for abort in aborts {
            assert_eq!(
                DurableComptimeFailure::abort(abort.clone()),
                DurableComptimeFailure::Abort(abort),
            );
        }

        let failure = DurableComptimeFailure::resolution(Arc::<str>::from("not ready"));
        assert!(matches!(
            failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message) if message.as_ref() == "not ready")
        ));

        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(rue_air::SemanticProviderError::Abort(
                QueryAbort::Canceled
            )),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(
                rue_air::SemanticProviderError::Failure(SemanticNucleusFailure::Resolution(
                    Arc::from("host failure")
                ))
            ),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            ))
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message) if message.as_ref() == "host failure")
        ));

        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(rue_air::SemanticProviderError::Abort(
                QueryAbort::Canceled
            )),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(
                rue_air::SemanticProviderError::Failure(SemanticNucleusFailure::Resolution(
                    Arc::from("provider failure")
                ))
            ),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            ))
                if matches!(value.as_ref(), SemanticNucleusFailure::Resolution(actual) if actual.as_ref() == "provider failure")
        ));
        assert!(matches!(
            DurableComptimeFailure::abort(QueryAbort::Canceled).into_host_error(),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
    }

    #[test]
    fn named_diagnostic_constructors_preserve_exact_legacy_text() {
        let cases = [
            (
                DurableComptimeFailure::maximum_depth(
                    "count",
                    rue_air::specialize::MAX_COMPTIME_CALL_DEPTH,
                ),
                format!(
                    "specialization of 'count' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                    rue_air::specialize::MAX_COMPTIME_CALL_DEPTH,
                ),
            ),
            (
                DurableComptimeFailure::integer_literal_overflow("i8", 128),
                "integer overflow evaluating constant at type i8: value 128 is out of range for type i8; 128 does not fit in i8 (this operation would panic at runtime)".to_owned(),
            ),
            (
                DurableComptimeFailure::arithmetic_overflow(
                    "i8",
                    "addition",
                    "value 128 is out of range for type i8; 128 does not fit in i8",
                ),
                "integer overflow evaluating addition at type i8: value 128 is out of range for type i8; 128 does not fit in i8 (this operation would panic at runtime)".to_owned(),
            ),
        ];
        for (failure, expected) in cases {
            let DurableComptimeFailure::Failure(value) = failure else {
                panic!("durable diagnostic must remain a domain failure");
            };
            let SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::ComptimeEvaluationFailed { reason: actual },
            ) = *value
            else {
                panic!("durable diagnostic has the wrong error channel");
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn unmatched_comptime_match_uses_the_canonical_legacy_failure() {
        assert!(matches!(
            DurableComptimeFailure::comptime_match_no_selected_arm(),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "comptime match has no selected arm")
        ));
    }

    #[test]
    fn projection_failures_preserve_typed_host_channels_and_legacy_text() {
        use crate::body_query::ComptimeProgramProjectionFailure as Projection;
        use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;
        use crate::revisioned_query_database::DeclarationBodyPlanFailure as Artifact;

        let producer = site("projection", 1, 2).producer;
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(Materialization::Query(
                QueryAbort::Canceled
            ))),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));

        let build = rue_rir::RirPayloadBuildError::ResourceLimitExceeded { family: "test" };
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(
                Materialization::Build(build)
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::CompilerResourceLimit(_)
            ))
        ));
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(
                Materialization::Invalid(Arc::from("invalid materialization"))
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == "invalid materialization")
        ));

        let artifact_build = rue_error::ErrorKind::InvalidInteger;
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(Artifact::Build(
                artifact_build.clone()
            ))),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::InvalidInteger
            ))
        ));
        let mut errors = crate::CompileErrors::new();
        errors.push(crate::CompileError::without_span(
            rue_error::ErrorKind::InvalidInteger,
        ));
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(
                Artifact::CandidateRirRejected(errors)
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Syntax(_))
        ));
        let unavailable = Artifact::CandidateUnavailable(producer.clone());
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(unavailable.clone())),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == format!("candidate RIR artifact failed: {unavailable:?}"))
        ));

        let query_failure = rue_query::QueryFailure::new("test-query", "payload");
        assert!(matches!(
            durable_projection_failure_error(Projection::ArtifactQueryFailure(
                query_failure.clone()
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryFailure(actual)
            )) if actual == query_failure
        ));

        for (failure, expected) in [
            (
                Projection::NotFunction {
                    root: rue_rir::InstRef::from_raw(0),
                },
                "comptime candidate artifact has a non-function root",
            ),
            (
                Projection::NotConst {
                    root: rue_rir::InstRef::from_raw(0),
                },
                "constant candidate artifact has a non-constant root",
            ),
        ] {
            assert!(matches!(
                durable_projection_failure_error(failure),
                rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                    DurableComptimeHostFailureKind::Semantic(value)
                )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                    if detail.as_ref() == expected)
            ));
        }

        let invalid_stable_key = crate::StableDefinitionKey::from_stable_parts(
            producer.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            producer.name.clone(),
            None,
        );
        let invalid_producer = Projection::InvalidProducer(invalid_stable_key);
        let invalid_debug = format!("{invalid_producer:?}");
        assert!(matches!(
            durable_projection_failure_error(invalid_producer),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == invalid_debug)
        ));
        let identity_mismatch = Projection::IdentityMismatch;
        let identity_debug = format!("{identity_mismatch:?}");
        assert!(matches!(
            durable_projection_failure_error(identity_mismatch),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == identity_debug)
        ));
    }

    #[test]
    fn semantic_rejection_kernel_preserves_each_legacy_channel_and_text() {
        let unit = EvaluatedSemanticConst::unit();
        let cases = [
            (
                ComptimeSemanticRejection::ConditionNotBoolean(unit.clone()),
                "comptime condition is not boolean",
            ),
            (
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Add,
                    lhs: unit.clone(),
                    rhs: Some(unit.clone()),
                },
                "comptime arithmetic operand is not an integer",
            ),
            (
                ComptimeSemanticRejection::UnaryOperandNotInteger(unit.clone()),
                "comptime arithmetic operand is not an integer",
            ),
            (
                ComptimeSemanticRejection::UnaryTypeNotInteger {
                    operation: ComptimeUnaryOperation::BitNot,
                    value: unit,
                },
                "comptime bitwise NOT operand is not an integer",
            ),
        ];
        for (rejection, expected) in cases {
            let DurableComptimeFailure::Failure(value) =
                DurableComptimeFailure::comptime_rejection(rejection)
            else {
                panic!("semantic rejection must remain a durable failure");
            };
            let SemanticNucleusFailure::Resolution(reason) = *value else {
                panic!("semantic rejection changed its failure channel");
            };
            assert_eq!(reason.as_ref(), expected);
        }
        for rejection in [
            ComptimeSemanticRejection::EmptyBlock,
            ComptimeSemanticRejection::UnsupportedExpression,
        ] {
            assert!(matches!(
                DurableComptimeFailure::comptime_rejection(rejection),
                DurableComptimeFailure::Failure(value)
                    if matches!(*value, SemanticNucleusFailure::Resolution(_))
            ));
        }
        let module = EvaluatedSemanticConst::Module(ModuleId::from_validated_canonical("m"));
        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Arch",
            variant: "X86_64",
        });
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ConditionNotBoolean(module)
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "module used where a value is required")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Add,
                    lhs: target,
                    rhs: None,
                }
            ),
                    DurableComptimeFailure::Failure(value)
                        if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "target descriptor used where a durable const value is required")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::AggregateExpression
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported { ref expr_kind }
                ) if expr_kind == "aggregate expression")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::UnsupportedIntrinsic("size_of".to_owned())
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported { ref expr_kind }
                ) if expr_kind == "intrinsic `@size_of`")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(ComptimeSemanticRejection::Assignment),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref()
                        == "assignment is not supported in declaration-time comptime")
        ));
    }

    #[test]
    fn semantic_rejection_kernel_preserves_noncommutative_operand_decisions() {
        let module = || EvaluatedSemanticConst::Module(ModuleId::from_validated_canonical("m"));
        let target = || {
            EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
                type_name: "Arch",
                variant: "X86_64",
            })
        };
        let boolean = || {
            EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Bool(true),
                DurableType::Bool,
            ))
        };
        let integer = || {
            EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(1),
                DurableType::I32,
            ))
        };
        let reason = |lhs, rhs| {
            let DurableComptimeFailure::Failure(value) = DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Lt,
                    lhs,
                    rhs,
                },
            ) else {
                panic!("expected durable failure");
            };
            let SemanticNucleusFailure::Resolution(reason) = *value else {
                panic!("expected resolution failure");
            };
            reason.to_string()
        };
        assert_eq!(
            reason(target(), Some(module())),
            "target descriptor comparison requires matching enum variants"
        );
        assert_eq!(
            reason(module(), Some(target())),
            "target descriptor comparison requires matching enum variants"
        );
        assert_eq!(
            reason(target(), Some(target())),
            "target descriptors support only equality comparisons"
        );
        assert_eq!(
            reason(boolean(), Some(boolean())),
            "boolean values support only equality comparisons"
        );
        assert_eq!(
            reason(boolean(), Some(integer())),
            "comptime arithmetic operand is not an integer"
        );
        assert_eq!(
            reason(integer(), Some(boolean())),
            "comptime arithmetic operand is not an integer"
        );
        assert_eq!(
            reason(target(), None),
            "target descriptor used where a durable const value is required"
        );
        assert_eq!(
            reason(module(), None),
            "module used where a value is required"
        );
    }

    #[test]
    fn stable_trap_site_uses_trap_range_and_distinguishes_colliding_owners() {
        let root = site("root", 10, 20);
        let nested = site("nested", 100, 120);
        let trap = |operation| rue_air::ComptimeTrap {
            operation,
            span: rue_span::Span::new(999, 1000),
        };
        let root_failure =
            DurableComptimeFailure::trap_at(root.producer.clone(), trap("division by zero"))
                .expect("division trap is supported");
        let nested_failure =
            DurableComptimeFailure::trap_at(nested.producer.clone(), trap("division by zero"))
                .expect("division trap is supported");
        assert!(matches!(
            root_failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 999, end: 1000, ..
                } if producer == &root.producer)
        ));
        assert!(matches!(
            nested_failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 999, end: 1000, ..
                } if producer == &nested.producer)
        ));
        let remainder =
            DurableComptimeFailure::trap_at(nested.producer.clone(), trap("remainder by zero"))
                .expect("remainder trap is supported");
        assert!(matches!(
            remainder,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer,
                    kind: rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason },
                    start: 999, end: 1000, ..
                } if producer == &nested.producer && reason == "remainder by zero (this operation would panic at runtime)"
            )
        ));
        assert!(DurableComptimeFailure::trap_at(nested.producer.clone(), trap("other")).is_none());
        assert_eq!((root.start, root.end), (10, 20));
        assert_eq!((nested.start, nested.end), (100, 120));
    }

    #[test]
    fn stable_site_constructor_keeps_explicit_non_trap_diagnostics() {
        let nested = site("nested", 100, 120);
        let failure = DurableComptimeFailure::comptime_failure_at(&nested, "foreign failure");
        assert!(matches!(
            failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 100, end: 120, ..
                } if producer == &nested.producer)
        ));
    }
}
