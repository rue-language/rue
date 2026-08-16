//! Host capabilities for canonical body analysis.
//!
//! The trait deliberately describes only the facts an analyzer host can answer.
//! It does not name the epoch, the analyzer, declaration phases, or any particular
//! storage strategy. The provider body host supplies these facts to executable
//! body analysis.

use ahash::AHashMap;

use lasso::Spur;
use rue_span::{FileId, Span};

use super::ConstValue;
use super::aggregate_resolution::AggregateFacts;
use super::body_endpoint::BodyEndpointProvider;
use super::call_resolution::CallResolutionFacts;
use crate::sema::inference_ctx::InferenceFactSource;
use crate::types::{ArrayLen, Type};

/// One exact declaration-owned type fragment in a body-local symbol domain.
/// Cloning this value only clones the arena's three shared slices; it never
/// renders source text or transfers a parser/interner identity.
#[derive(Debug, Clone)]
pub(crate) struct StructuredTypeSyntax {
    pub(crate) arena: rue_rir::RirTypeSyntaxArena<Spur>,
    pub(crate) root: rue_rir::RirTypeSyntaxRef,
}

pub(crate) struct StructuredTypeSyntaxRequest<'a> {
    pub(crate) syntax: &'a StructuredTypeSyntax,
    pub(crate) root_file: FileId,
    pub(crate) span: Span,
    pub(crate) type_substitutions: Option<&'a AHashMap<Spur, Type>>,
    pub(crate) value_substitutions: Option<&'a AHashMap<Spur, ConstValue>>,
}

/// Exact input for resolving a module-qualified type prefix.
pub(crate) struct ModulePrefixRequest<'a> {
    pub(crate) root_file: FileId,
    pub(crate) segments: &'a [&'a str],
    pub(crate) span: Span,
}

/// Exact input for resolving a compile-time array length.
pub(crate) struct ArrayLengthRequest<'a> {
    pub(crate) length: &'a ArrayLen,
    pub(crate) span: Span,
    pub(crate) value_substitutions: Option<&'a AHashMap<Spur, ConstValue>>,
}

pub(crate) type TypeSyntaxResult = Result<
    Type,
    crate::SemanticTypeSyntaxError<std::convert::Infallible, rue_error::CompileError, FileId, Spur>,
>;

/// The read-only capabilities required by body analysis.
///
/// This marker adds no object or dispatch layer; it groups the contracts both
/// concrete hosts implement directly.
pub(crate) trait BodyAnalysisReadHost:
    BodyEndpointProvider + CallResolutionFacts + AggregateFacts + InferenceFactSource
{
}

impl<T> BodyAnalysisReadHost for T where
    T: BodyEndpointProvider + CallResolutionFacts + AggregateFacts + InferenceFactSource
{
}
