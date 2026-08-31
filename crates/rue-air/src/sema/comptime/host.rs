use super::*;

/// Error classification for fallible semantic host operations. Ordinary host
/// failures are distinct from query cancellation/abort so the engine can
/// preserve aborts through entered frames and keep them out of memoization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComptimeHostError<F> {
    HostFailure(F),
    Abort(F),
}

pub type ComptimeHostResult<T, F> = Result<T, ComptimeHostError<F>>;

impl<F> From<F> for ComptimeHostError<F> {
    fn from(value: F) -> Self {
        Self::HostFailure(value)
    }
}

impl<F> ComptimeHostError<F> {
    pub(crate) fn into_failure(self) -> F {
        match self {
            Self::HostFailure(error) | Self::Abort(error) => error,
        }
    }
}

/// Semantic host boundary for the canonical dispatcher. No method accepts an
/// instruction callback or a child RIR reference for evaluation.
pub trait ComptimeHostTypes {
    type Type: ComptimeType;
    type Value: ComptimeValue<Type = Self::Type>;
    type Name: ComptimeName;
    type File: ComptimeFile;
    type CanonicalIdentity: ComptimeIdentity;
    type AnonymousIdentity: ComptimeIdentity;
    type ProgramKey: Clone;
    type Failure;
    type CallAdmission;
    /// Host-owned, non-replayable binding state. The engine creates one state
    /// immediately after admission and feeds it source-order arguments before
    /// evaluating the next child.
    type CallBinding;
    /// Opaque, host-owned completed binding. The engine does not reconstruct
    /// ordered arguments or couple preparation to a map representation.
    type BoundCall;
    /// Opaque host-owned completion state issued during ordered preparation.
    type CompletionTicket;
    /// The sole continuation representation accepted by the engine for a
    /// structured type reduction. This is sealed below to prevent a peer
    /// resolver state machine from being hidden behind the host boundary.
    type StructuredTypeSuspension: ComptimeStructuredTypeSuspension;
}

/// Program snapshots, symbol identities, cancellation, and atomic named-value
/// lookup required by the canonical evaluator.
pub trait ComptimeProgramHost: ComptimeHostTypes {
    /// Check the owning query's cancellation state before reading any RIR for
    /// an evaluation node. This is deliberately required so every host makes
    /// abort semantics explicit; the engine performs the checkpoint exactly
    /// once at the entry to `eval`.
    fn check_canceled(&self) -> ComptimeHostResult<(), Self::Failure>;
    fn program_rir(&self, program: &Self::ProgramKey) -> &Rir;
    fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name;
    fn display_name(&self, name: &Self::Name) -> String;
    fn file_for_program_span(&self, program: &Self::ProgramKey, span: &Span) -> Self::File;
    fn resolve_comptime_named_value(
        &mut self,
        file: Self::File,
        name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<ComptimeNamedValueResolution<Self::Value>, Self::Failure>;
}

/// Value reduction, diagnostics, control selection, and checked-integer
/// policies required by the canonical evaluator.
pub trait ComptimeValueHost: ComptimeHostTypes {
    fn match_pattern(
        &self,
        pattern: &ComptimeMatchPattern<Self::Name>,
        value: &Self::Value,
    ) -> Option<bool>;
    /// Resolve the terminal policy when every reached match arm declined.
    /// Ordinary body evaluation remains runtime-dependent; durable hosts may
    /// preserve a declaration-time failure through this semantic hook.
    fn match_no_selected_arm(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    fn reject_comptime_expression(
        &self,
        rejection: ComptimeSemanticRejection<Self::Value>,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    /// Whether a durable semantic host needs both source-order operands before
    /// validating an integer operation. Ordinary body evaluation short-circuits
    /// after a known invalid lhs; durable declaration evaluation preserves its
    /// historical evaluate-both-before-validation order.
    fn evaluate_binary_rhs_after_rejection(&self) -> bool;
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn depth_exceeded(
        &self,
        name: &Self::Name,
        depth: usize,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn literal_out_of_range(
        &self,
        value: u64,
        ty: &Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn float_not_implemented(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn cannot_negate(
        &self,
        ty: &Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    /// Give a semantic host a chance to preserve a checked-negation policy
    /// after the operand has been reduced. Ordinary hosts retain the
    /// historical immediate `CannotNegate` terminal; durable hosts may defer
    /// to their checked integer-result policy.
    fn reject_unsigned_negation(
        &self,
        ty: &Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Option<Self::Failure> {
        Some(self.cannot_negate(ty, site))
    }
    fn unsupported_anon_method_type_param(
        &self,
        method_name: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn non_function_anon_method(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure;
    fn const_expr_type(
        &self,
        program: &Self::ProgramKey,
        env: &ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        inst_ref: InstRef,
    ) -> Option<Self::Type>;

    /// Compare values that are not represented by the generic integer/bool
    /// algebra (for example target descriptors). The ordinary body domain
    /// keeps those comparisons runtime-dependent.
    fn compare_comptime_values(
        &mut self,
        _lhs: &Self::Value,
        _rhs: &Self::Value,
        _equal: bool,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure>;

    /// Select the integer type for a binary operation. The default preserves
    /// the existing resolved-type lookup; durable hosts can fall back to the
    /// typed metadata carried by the reduced operands without inspecting RIR.
    fn integer_operation_type(
        &self,
        resolved_type: Option<&Self::Type>,
        lhs: &Self::Value,
        rhs: &Self::Value,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        Ok(resolved_type
            .cloned()
            .or_else(|| lhs.as_integer_type())
            .or_else(|| rhs.as_integer_type()))
    }

    /// Select the integer type for a unary operation. A durable host can
    /// preserve the operand's type metadata after the child has been reduced,
    /// while the default retains the ordinary resolved-type lookup.
    fn unary_integer_type(
        &self,
        resolved_type: Option<&Self::Type>,
        operand: &Self::Value,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        Ok(resolved_type.cloned().or_else(|| operand.as_integer_type()))
    }
}

/// Type construction, type intrinsics, and semantic type lookup required by
/// the canonical evaluator.
pub trait ComptimeTypeHost: ComptimeHostTypes {
    fn resolve_named_array_length(
        &mut self,
        name: &Self::Name,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        // The historical substitution view used by ordinary hosts. The
        // engine separately supplies `binding`, which classifies lexical
        // locals/runtime shadows before any host or global lookup.
        values: Option<&AHashMap<Self::Name, Self::Value>>,
        binding: ComptimeArrayLengthBinding<Self::Value>,
    ) -> ComptimeOutcome<u64, Self::Failure>;
    fn rir_type_named_symbol(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Self::Name>;
    fn get_or_create_array_type(&mut self, element: Self::Type, length: u64) -> Self::Type;
    fn find_or_create_anon_struct(
        &mut self,
        identity: Self::AnonymousIdentity,
        fields: &[ComptimeField<Self::Name, Self::Type>],
        sigs: &[ComptimeMethodDescriptor<Self::Name, Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure>;
    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<Self::Type, Self::Failure>;
    fn check_require_droppable(
        &mut self,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn check_trivially_droppable(
        &mut self,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn type_name(&self, ty: &Self::Type) -> String;
    fn type_is_unsigned(&self, ty: &Self::Type) -> bool;
    fn type_integer_semantics(&self, ty: &Self::Type) -> Option<IntegerType>;
    fn resolve_named_type_value(
        &mut self,
        program: &Self::ProgramKey,
        _name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure>;
    fn resolve_comptime_type_path(
        &mut self,
        file: Self::File,
        segments: &[Self::Name],
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure>;
    fn render_rir_type(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> String;
    /// Resolve a classified type intrinsic after its type argument has been
    /// reduced. The default delegates to the ordinary ownership hooks and
    /// integer-bound behavior; durable hosts can override this one typed seam
    /// to preserve their immediate mismatch diagnostics.
    fn resolve_comptime_type_intrinsic(
        &mut self,
        intrinsic: ComptimeTypeIntrinsic,
        ty: Self::Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        match intrinsic {
            ComptimeTypeIntrinsic::RequireDroppable => {
                self.check_require_droppable(ty, site)?;
                Ok(Some(Self::Value::unit()))
            }
            ComptimeTypeIntrinsic::RequireTriviallyDroppable => {
                self.check_trivially_droppable(ty, site)?;
                Ok(Some(Self::Value::unit()))
            }
            ComptimeTypeIntrinsic::IntegerBound(bound) => {
                let Some(integer) = self.type_integer_semantics(&ty) else {
                    return Ok(None);
                };
                let value = match bound {
                    ComptimeIntegerBound::Max => integer.max_i128(),
                    ComptimeIntegerBound::Min => integer.min_i128(),
                };
                Ok(Some(Self::Value::integer_typed(value, Some(ty))))
            }
        }
    }
}

/// Admission, ordered binding, entry, completion, and memo lookup for nested
/// compile-time calls. Storage remains entirely host-owned.
pub trait ComptimeCallHost: ComptimeHostTypes {
    fn resolve_module_comptime_callable(
        &mut self,
        file_id: Self::File,
        segments: &[Self::Name],
        method: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::Name>, Self::Failure>;
    fn comptime_method_receiver_policy(&self) -> ComptimeMethodReceiverPolicy {
        ComptimeMethodReceiverPolicy::SyntacticModulePath
    }
    /// Admit a method call after its receiver has been evaluated. The
    /// receiver remains in the host-owned admission payload, so a durable
    /// host cannot accidentally resolve the method against an unqualified
    /// spelling in the caller's module.
    fn admit_evaluated_comptime_method(
        &mut self,
        _receiver: Self::Value,
        _method: Self::Name,
        _arg_count: usize,
        _arg_modes: &[ComptimeArgMode],
        _env: &mut ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        ComptimeOutcome::RuntimeDependent
    }
    fn admit_comptime_call(
        &mut self,
        name: Self::Name,
        arg_count: usize,
        arg_modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        name_is_resolved_key: bool,
    ) -> ComptimeHostResult<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    >;
    fn begin_comptime_call_binding(
        &self,
        admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        argument_count: usize,
        span: Span,
    ) -> ComptimeHostResult<Self::CallBinding, Self::Failure>;
    /// Push one already-evaluated argument. `false` rejects the call as
    /// runtime-dependent and stops the engine before the next child runs.
    fn bind_comptime_call_argument(
        &self,
        binding: &mut Self::CallBinding,
        argument: ComptimeCallArgument<Self::Value>,
        index: usize,
        span: Span,
    ) -> ComptimeHostResult<bool, Self::Failure>;
    fn finish_comptime_call_binding(
        &mut self,
        binding: Self::CallBinding,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::BoundCall>, Self::Failure>;
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        bound: Self::BoundCall,
        span: Span,
    ) -> ComptimeHostResult<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    >;
    /// Look up a successful result in the host's evaluation-local completed
    /// memo after the engine has admitted this frame's depth and canonical
    /// identity. A hit returns directly without activating or finishing the
    /// completion ticket. Durable hosts retain the default miss behavior;
    /// ordinary body hosts may use this hook for their body-local memo.
    fn lookup_completed_comptime_call(
        &mut self,
        _frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        Ok(None)
    }
    fn finish_comptime_call(
        &mut self,
        frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        ticket: Self::CompletionTicket,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure>;
    /// Activate a prepared completion ticket only after the engine has
    /// admitted depth and has a canonical producer identity. A host may issue
    /// that identity during preparation when it also needs it for admission;
    /// the engine then carries it into the entered frame.
    fn enter_comptime_call(
        &mut self,
        _frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        _ticket: &Self::CompletionTicket,
    ) -> ComptimeHostResult<(), Self::Failure>;
    fn label_ctor_instantiation_site(error: Self::Failure, call_span: Span) -> Self::Failure;
    fn canonical_function_producer(
        &self,
        program: &Self::ProgramKey,
        ticket: &Self::CompletionTicket,
        name: Self::Name,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure>;
    fn issue_anonymous_identity(
        &self,
        program: &Self::ProgramKey,
        kind: ComptimeAnonymousKind,
        producer: &Self::CanonicalIdentity,
        anchor: &rue_rir::RirStructuralAnchor,
    ) -> Self::AnonymousIdentity;
}

/// Structured-type suspension and resumption on the evaluator's existing call
/// stack, without exposing syntax traversal to the execution core.
pub trait ComptimeStructuredTypeHost: ComptimeHostTypes {
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> Option<Self::Type>;

    fn prepare_structured_type_call(
        &mut self,
        suspension: &Self::StructuredTypeSuspension,
        span: Span,
    ) -> ComptimeOutcome<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    >;

    fn resume_structured_type_call(
        &mut self,
        suspension: Self::StructuredTypeSuspension,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    >;

    /// Begin a structured type reduction. The default is the staged
    /// synchronous adapter; an admitted keyed host may return a canonical
    /// suspension here.
    fn begin_comptime_type_syntax(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        span: Span,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        self.resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            program, syntax, types, values, span,
        )
        .map_or(ComptimeOutcome::RuntimeDependent, |value| {
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value))
        })
    }
}

/// Typed expression intrinsics, enum/member semantics, and checked-expression
/// policies required after canonical decoding.
pub trait ComptimeIntrinsicHost: ComptimeHostTypes {
    /// Resolve a string literal in a semantic context. The ordinary body
    /// value domain has no compile-time string value, so the default keeps
    /// string expressions runtime-dependent. Durable hosts may use this hook
    /// for controls such as `@import` without inspecting the instruction.
    fn resolve_string_const(
        &mut self,
        _content: Self::Name,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Resolve a classified expression intrinsic after AIR has decoded its
    /// exact argument shape. No child argument is evaluated for this finite
    /// family; durable hosts can perform the keyed semantic operation directly.
    fn resolve_comptime_expression_intrinsic(
        &mut self,
        _request: ComptimeExpressionIntrinsicRequest<Self::Name>,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Resolve a discriminant-only or payload-bearing enum variant after the
    /// optional module expression has been reduced by the engine. The default
    /// preserves ordinary body behavior, where enum values are runtime data.
    fn resolve_comptime_enum_variant(
        &mut self,
        _module: Option<Self::Value>,
        _type_name: Self::Name,
        _variant: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    fn admit_comptime_enum_variant(
        &mut self,
        _type_name: Self::Name,
        _variant: Self::Name,
        _has_module: bool,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(false)
    }

    fn admit_comptime_member(
        &mut self,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(false)
    }

    fn resolve_comptime_member(
        &mut self,
        _base: Self::Value,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }

    /// Preserve checked-block semantics after the child has been evaluated by
    /// the engine. A durable host can attach its own context observation while
    /// the default remains a transparent wrapper.
    fn finish_checked(
        &mut self,
        value: Self::Value,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::Known(value)
    }

    /// Ordinary body analysis has historically treated `checked { ... }` as
    /// runtime-only during comptime probing. Durable declaration hosts opt in
    /// once they can preserve the checked-context observation.
    fn allow_checked_comptime(&self) -> bool {
        false
    }

    /// Give a durable host a typed rejection point for a non-type array
    /// repeat. The existing engine only folds repeats whose element is a type;
    /// ordinary body evaluation therefore remains runtime-dependent by
    /// default.
    fn reject_non_type_array_repeat(
        &mut self,
        _value: Self::Value,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }
}

/// Empty umbrella preserving the single bound used by the execution core.
pub trait ComptimeHost:
    ComptimeProgramHost
    + ComptimeValueHost
    + ComptimeTypeHost
    + ComptimeCallHost
    + ComptimeStructuredTypeHost
    + ComptimeIntrinsicHost
{
}

impl<T> ComptimeHost for T where
    T: ComptimeProgramHost
        + ComptimeValueHost
        + ComptimeTypeHost
        + ComptimeCallHost
        + ComptimeStructuredTypeHost
        + ComptimeIntrinsicHost
{
}
