//! Short-lived fact facades for canonical one-body semantic analysis.
//!
//! `EpochFactMode` is the only production mode. It preserves the current
//! analyzer while making its existing fact seams explicit.

use super::ConstValue;
use super::aggregate_resolution::{AggregateFacts, EpochFacts as EpochAggregateFacts};
use super::body_endpoint::{BodyEndpointProvider, EpochFacts as EpochEndpointFacts};
use super::call_resolution::{CallResolutionFacts, EpochFacts as EpochCallFacts};
use super::typeck::SemaTypeRootAuthority;
use super::{BodySema, DeclarationPhase, InferenceContext, Sema, SemaInferenceFacts};
use crate::inference::LazyInferenceFacts;
use crate::types::{ArrayLen, ModuleId, Type};
use lasso::Spur;
use rue_span::Span;
use std::collections::HashMap;

/// Selects the short-lived endpoint and call-resolution facades used by the
/// one-body engine. A facade borrows the analyzer only for one resolution.
pub(crate) trait BodyAnalysisFactMode: Sized {
    type EndpointFacts<'s, 'a>: BodyEndpointProvider
    where
        Self: 's,
        'a: 's;

    fn endpoint_facts<'s, 'a>(
        &'s self,
        sema: &'s BodySema<'a, Self>,
    ) -> Self::EndpointFacts<'s, 'a>;

    type CallFacts<'s, 'a>: CallResolutionFacts
    where
        Self: 's,
        'a: 's;

    fn call_facts<'s, 'a>(&'s self, sema: &'s BodySema<'a, Self>) -> Self::CallFacts<'s, 'a>;

    type AggregateFacts<'s, 'a, D: DeclarationPhase>: AggregateFacts
    where
        Self: 's,
        D: 's,
        'a: 's;

    fn aggregate_facts<'s, 'a, D: DeclarationPhase>(
        &'s self,
        sema: &'s Sema<'a, D, Self>,
    ) -> Self::AggregateFacts<'s, 'a, D>;

    type InferenceFacts<'s, 'a, D: DeclarationPhase>: LazyInferenceFacts
    where
        Self: 's,
        D: 's,
        'a: 's;

    fn inference_facts<'s, 'a, D: DeclarationPhase>(
        &'s self,
        ctx: &'s InferenceContext,
        sema: &'s Sema<'a, D, Self>,
    ) -> Self::InferenceFacts<'s, 'a, D>;

    fn resolve_type_syntax_with_substitutions<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        syntax: &str,
        root_file: rue_span::FileId,
        root_authority: SemaTypeRootAuthority,
        span: Span,
        type_substitutions: Option<&HashMap<Spur, Type>>,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> Result<
        Type,
        crate::SemanticTypeSyntaxError<
            std::convert::Infallible,
            rue_error::CompileError,
            rue_span::FileId,
            Spur,
        >,
    >;

    fn resolve_type_module_prefix<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        root_file: rue_span::FileId,
        segments: &[&str],
        span: Span,
    ) -> rue_error::CompileResult<(ModuleId, Option<rue_span::FileId>, String)>;

    fn resolve_array_length_fact<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        length: &ArrayLen,
        span: Span,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> rue_error::CompileResult<u64>;

    fn validate_deferred_type_position<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        type_name: String,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> rue_error::CompileResult<Option<Type>>;

    #[allow(clippy::too_many_arguments)]
    fn validate_deferred_value_position<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        value_name: &str,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        span: Span,
    ) -> rue_error::CompileResult<Option<ConstValue>>;
}

/// Epoch-backed facts are the sole production configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct EpochFactMode;

impl BodyAnalysisFactMode for EpochFactMode {
    type EndpointFacts<'s, 'a>
        = EpochEndpointFacts<'s, 'a>
    where
        Self: 's,
        'a: 's;

    fn endpoint_facts<'s, 'a>(
        &'s self,
        sema: &'s BodySema<'a, Self>,
    ) -> Self::EndpointFacts<'s, 'a> {
        EpochEndpointFacts::new(sema)
    }

    type CallFacts<'s, 'a>
        = EpochCallFacts<'s, 'a>
    where
        Self: 's,
        'a: 's;

    fn call_facts<'s, 'a>(&'s self, sema: &'s BodySema<'a, Self>) -> Self::CallFacts<'s, 'a> {
        EpochCallFacts::new(sema)
    }

    type AggregateFacts<'s, 'a, D: DeclarationPhase>
        = EpochAggregateFacts<'s, 'a, D>
    where
        Self: 's,
        D: 's,
        'a: 's;

    fn aggregate_facts<'s, 'a, D: DeclarationPhase>(
        &'s self,
        sema: &'s Sema<'a, D, Self>,
    ) -> Self::AggregateFacts<'s, 'a, D> {
        EpochAggregateFacts::new(sema)
    }

    type InferenceFacts<'s, 'a, D: DeclarationPhase>
        = SemaInferenceFacts<'s, 'a, D>
    where
        Self: 's,
        D: 's,
        'a: 's;

    fn inference_facts<'s, 'a, D: DeclarationPhase>(
        &'s self,
        ctx: &'s InferenceContext,
        sema: &'s Sema<'a, D, Self>,
    ) -> Self::InferenceFacts<'s, 'a, D> {
        SemaInferenceFacts::new(ctx, sema)
    }

    fn resolve_type_syntax_with_substitutions<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        syntax: &str,
        root_file: rue_span::FileId,
        root_authority: SemaTypeRootAuthority,
        span: Span,
        type_substitutions: Option<&HashMap<Spur, Type>>,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> Result<
        Type,
        crate::SemanticTypeSyntaxError<
            std::convert::Infallible,
            rue_error::CompileError,
            rue_span::FileId,
            Spur,
        >,
    > {
        sema.resolve_type_syntax_with_epoch_facts(
            syntax,
            root_file,
            root_authority,
            span,
            type_substitutions,
            value_substitutions,
        )
    }

    fn resolve_type_module_prefix<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        root_file: rue_span::FileId,
        segments: &[&str],
        span: Span,
    ) -> rue_error::CompileResult<(ModuleId, Option<rue_span::FileId>, String)> {
        sema.resolve_type_module_prefix_with_epoch_facts(root_file, segments, span)
    }

    fn resolve_array_length_fact<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        length: &ArrayLen,
        span: Span,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> rue_error::CompileResult<u64> {
        sema.resolve_array_length_with_epoch_facts(length, span, value_substitutions)
    }

    fn validate_deferred_type_position<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        type_name: String,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> rue_error::CompileResult<Option<Type>> {
        sema.validate_deferred_type_position_with_epoch_facts(
            type_name,
            type_params,
            value_params,
            value_param_type_syms,
            span,
        )
    }

    fn validate_deferred_value_position<'a, D: DeclarationPhase>(
        &self,
        sema: &mut Sema<'a, D, Self>,
        value_name: &str,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        span: Span,
    ) -> rue_error::CompileResult<Option<ConstValue>> {
        sema.validate_deferred_value_position_with_epoch_facts(
            value_name,
            type_params,
            value_params,
            value_param_type_syms,
            expected,
            contract,
            require_integer,
            span,
        )
    }
}
