//! Host capabilities for canonical body analysis.
//!
//! The trait deliberately describes only the facts an analyzer host can answer.
//! It does not name the epoch, `Sema`, declaration phases, or any particular
//! storage strategy. The provider body host supplies these facts to executable
//! body analysis.

use std::collections::HashMap;

use lasso::Spur;
use rue_span::{FileId, Span};

use super::aggregate_resolution::{
    AggregateFactSource, AggregateFacts, EpochFacts as EpochAggregateFacts,
};
use super::body_endpoint::{
    BodyEndpointFactSource, BodyEndpointProvider, EpochFacts as EpochEndpointFacts,
};
use super::call_resolution::{
    CallResolutionFactSource, CallResolutionFacts, EpochFacts as EpochCallFacts,
};
use super::{ConstValue, DeclarationPhase, HostInferenceFacts, InferenceContext, Sema};
use crate::inference::LazyInferenceFacts;
use crate::sema::inference_ctx::InferenceFactSource;
use crate::types::{ArrayLen, ModuleId, Type};

/// Exact input for resolving one type-syntax fragment.
///
/// `root_file` is the whole scope of the request: a source name is resolved
/// against that file's declarations and imports and nothing wider. There is
/// deliberately no way to ask for a scope-free lookup (RUE-497, RUE-1126).
pub(crate) struct TypeSyntaxRequest<'a> {
    pub(crate) syntax: &'a str,
    pub(crate) root_file: FileId,
    pub(crate) span: Span,
    pub(crate) type_substitutions: Option<&'a HashMap<Spur, Type>>,
    pub(crate) value_substitutions: Option<&'a HashMap<Spur, ConstValue>>,
}

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
    pub(crate) type_substitutions: Option<&'a HashMap<Spur, Type>>,
    pub(crate) value_substitutions: Option<&'a HashMap<Spur, ConstValue>>,
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
    pub(crate) value_substitutions: Option<&'a HashMap<Spur, ConstValue>>,
}

type TypeSyntaxResult = Result<
    Type,
    crate::SemanticTypeSyntaxError<std::convert::Infallible, rue_error::CompileError, FileId, Spur>,
>;

/// The read-only slice of a body-analysis host.
///
/// The adapters below only need these immutable point-query capabilities. The
/// mutable receiver/state migration is deliberately outside this extraction.
pub(crate) trait BodyAnalysisReadHost:
    BodyEndpointFactSource + CallResolutionFactSource + AggregateFactSource + InferenceFactSource
{
}

impl<T> BodyAnalysisReadHost for T where
    T: BodyEndpointFactSource
        + CallResolutionFactSource
        + AggregateFactSource
        + InferenceFactSource
{
}

/// Host capability consumed by the canonical body-analysis engine.
///
/// Associated fact facades return owned or short-lived read-only values; the
/// mutable operations take a request object so the contract stays independent
/// of the current analyzer representation.
pub(crate) trait BodyAnalysisHost: BodyAnalysisReadHost + Sized {
    type EndpointFacts<'a>: BodyEndpointProvider
    where
        Self: 'a;
    fn endpoint_facts(&self) -> Self::EndpointFacts<'_>;

    type CallFacts<'a>: CallResolutionFacts
    where
        Self: 'a;
    fn call_facts(&self) -> Self::CallFacts<'_>;

    type AggregateFacts<'a>: AggregateFacts
    where
        Self: 'a;
    fn aggregate_facts(&self) -> Self::AggregateFacts<'_>;

    type InferenceFacts<'a>: LazyInferenceFacts
    where
        Self: 'a;
    fn inference_facts<'a>(&'a self, ctx: &'a InferenceContext) -> Self::InferenceFacts<'a>;

    fn resolve_type_syntax(&mut self, request: TypeSyntaxRequest<'_>) -> TypeSyntaxResult;
    fn resolve_structured_type_syntax(
        &mut self,
        request: StructuredTypeSyntaxRequest<'_>,
    ) -> TypeSyntaxResult;
    fn resolve_type_module_prefix(
        &mut self,
        request: ModulePrefixRequest<'_>,
    ) -> rue_error::CompileResult<(ModuleId, Option<FileId>, String)>;
    fn resolve_array_length(
        &mut self,
        request: ArrayLengthRequest<'_>,
    ) -> rue_error::CompileResult<u64>;
}

/// The canonical epoch host. This is the only location that names the epoch
/// adapters; the host contract above remains representation-independent.
impl<'source, D: DeclarationPhase> BodyAnalysisHost for Sema<'source, D> {
    type EndpointFacts<'a>
        = EpochEndpointFacts<'a, Self>
    where
        Self: 'a;

    fn endpoint_facts(&self) -> Self::EndpointFacts<'_> {
        EpochEndpointFacts::new(self)
    }

    type CallFacts<'a>
        = EpochCallFacts<'a, Self>
    where
        Self: 'a;

    fn call_facts(&self) -> Self::CallFacts<'_> {
        EpochCallFacts::new(self)
    }

    type AggregateFacts<'a>
        = EpochAggregateFacts<'a, Self>
    where
        Self: 'a;

    fn aggregate_facts(&self) -> Self::AggregateFacts<'_> {
        EpochAggregateFacts::new(self)
    }

    type InferenceFacts<'a>
        = HostInferenceFacts<'a, Self>
    where
        Self: 'a;

    fn inference_facts<'a>(&'a self, ctx: &'a InferenceContext) -> Self::InferenceFacts<'a> {
        HostInferenceFacts::new(ctx, self)
    }

    fn resolve_type_syntax(&mut self, request: TypeSyntaxRequest<'_>) -> TypeSyntaxResult {
        self.resolve_type_syntax_with_epoch_facts(
            request.syntax,
            request.root_file,
            request.span,
            request.type_substitutions,
            request.value_substitutions,
        )
    }

    fn resolve_structured_type_syntax(
        &mut self,
        request: StructuredTypeSyntaxRequest<'_>,
    ) -> TypeSyntaxResult {
        self.resolve_structured_type_syntax_with_epoch_facts(
            &request.syntax.arena,
            request.syntax.root,
            request.root_file,
            request.span,
            request.type_substitutions,
            request.value_substitutions,
        )
    }

    fn resolve_type_module_prefix(
        &mut self,
        request: ModulePrefixRequest<'_>,
    ) -> rue_error::CompileResult<(ModuleId, Option<FileId>, String)> {
        self.resolve_type_module_prefix_with_epoch_facts(
            request.root_file,
            request.segments,
            request.span,
        )
    }

    fn resolve_array_length(
        &mut self,
        request: ArrayLengthRequest<'_>,
    ) -> rue_error::CompileResult<u64> {
        self.resolve_array_length_with_epoch_facts(
            request.length,
            request.span,
            request.value_substitutions,
        )
    }
}

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_rir::InstRef;

    use super::*;
    use crate::inference::{FunctionSig, MethodSig};
    use crate::sema::PreviewFeatures;
    use crate::sema::aggregate_resolution::AggregateModuleFact;
    use crate::sema::anon_structs::IssuedAnonymousNominalKey;
    use crate::sema::info::{
        ConstInfo, FunctionCallInfo, FunctionInfo, MethodCallInfo, MethodInfo,
    };
    use crate::types::{EnumId, ModuleDef, StructId};
    use crate::{
        SemanticDefinitionEndpoint, SemanticDefinitionToken, SemanticModuleEndpoint,
        SemanticModuleToken,
    };

    /// A deliberately non-`Clone` owned test sentinel.
    struct OwnedHostSentinel {
        tag: u8,
    }

    impl OwnedHostSentinel {
        fn tag(&self) -> u8 {
            self.tag
        }
    }

    /// An owned read host. It models a tiny stable slice rather than wrapping
    /// an analyzer.
    struct OwnedReadHost {
        ownership: OwnedHostSentinel,
        label: Box<str>,
        symbol: Spur,
        path: Box<str>,
    }

    impl BodyEndpointFactSource for OwnedReadHost {
        fn endpoint_name_symbol(&self, name: &str) -> Option<Spur> {
            (name == self.label.as_ref()).then_some(self.symbol)
        }
        fn endpoint_definition_endpoint(
            &self,
            _: SemanticDefinitionToken,
        ) -> Option<SemanticDefinitionEndpoint> {
            None
        }
        fn endpoint_module_endpoint(
            &self,
            _: SemanticModuleToken,
        ) -> Option<SemanticModuleEndpoint> {
            None
        }
        fn endpoint_function_by_file_name(&self, _: FileId, _: Spur) -> Option<Spur> {
            None
        }
        fn endpoint_struct_by_file_name(&self, _: FileId, _: Spur) -> Option<StructId> {
            None
        }
        fn endpoint_enum_by_file_name(&self, _: FileId, _: Spur) -> Option<EnumId> {
            None
        }
        fn endpoint_builtin_or_generated_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn endpoint_generated_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn endpoint_builtin_enum(&self, _: Spur) -> Option<EnumId> {
            None
        }
        fn endpoint_anon_struct(&self, _: &IssuedAnonymousNominalKey) -> Option<StructId> {
            None
        }
        fn endpoint_anon_enum(&self, _: &IssuedAnonymousNominalKey) -> Option<EnumId> {
            None
        }
        fn endpoint_function_info(&self, _: Spur) -> Option<FunctionInfo> {
            None
        }
        fn endpoint_method_info(&self, _: StructId, _: Spur) -> Option<MethodInfo> {
            None
        }
        fn endpoint_source_function_name(&self, name: Spur) -> Spur {
            name
        }
        fn endpoint_module_id_for_file(&self, _: u32) -> Option<ModuleId> {
            None
        }
        fn endpoint_intern_array(&self, _: Type, _: u64) -> Option<Type> {
            None
        }
        fn endpoint_intern_ptr_const(&self, _: Type) -> Option<Type> {
            None
        }
        fn endpoint_intern_ptr_mut(&self, _: Type) -> Option<Type> {
            None
        }
    }

    impl CallResolutionFactSource for OwnedReadHost {
        fn call_function_info(&self, _: Spur) -> Option<FunctionCallInfo> {
            None
        }
        fn call_function_contains(&self, _: Spur) -> bool {
            false
        }
        fn call_source_function_name(&self, name: Spur) -> Spur {
            name
        }
        fn call_resolve_function_name_local(&self, _: Spur, _: FileId) -> Option<Spur> {
            None
        }
        fn call_resolve_const_info_in_file(&self, _: Spur, _: FileId) -> Option<ConstInfo> {
            None
        }
        fn call_value_const(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn call_module_binding(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn call_method_info(&self, _: StructId, _: Spur) -> Option<MethodCallInfo> {
            None
        }
        fn call_named_method_declaration(&self, _: FileId, _: Spur, _: Spur) -> Option<InstRef> {
            None
        }
        fn call_module_def(&self, _: ModuleId) -> ModuleDef {
            unreachable!("test host has no modules")
        }
    }

    impl AggregateFactSource for OwnedReadHost {
        fn aggregate_value_const(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn aggregate_module_binding(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn aggregate_struct_in_file(&self, _: FileId, _: Spur) -> Option<StructId> {
            None
        }
        fn aggregate_enum_in_file(&self, _: FileId, _: Spur) -> Option<EnumId> {
            None
        }
        fn aggregate_builtin_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn aggregate_builtin_enum(&self, _: Spur) -> Option<EnumId> {
            None
        }
        fn aggregate_module(&self, _: ModuleId) -> AggregateModuleFact {
            unreachable!("test host has no modules")
        }
        fn aggregate_file_path(&self, file: FileId) -> Option<&str> {
            (file == FileId::DEFAULT).then_some(self.path.as_ref())
        }
        fn aggregate_source_path(&self, _: Span) -> Option<&str> {
            None
        }
    }

    impl InferenceFactSource for OwnedReadHost {
        fn inference_generated_nominal_overlays(
            &self,
        ) -> super::super::inference_ctx::InferenceGeneratedNominalOverlays {
            super::super::inference_ctx::InferenceGeneratedNominalOverlays {
                builtin_struct_types: std::collections::HashMap::new(),
                struct_types_by_file: std::collections::HashMap::new(),
                enum_types_by_file: std::collections::HashMap::new(),
            }
        }
        fn uncached_function_sig(&self, _: Spur) -> Option<FunctionSig> {
            None
        }
        fn uncached_method_sig(&self, _: (StructId, Spur)) -> Option<MethodSig> {
            None
        }
        fn inference_builtin_struct_type(&self, _: Spur) -> Option<Type> {
            None
        }
        fn inference_struct_type_by_file(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn inference_builtin_enum_type(&self, _: Spur) -> Option<Type> {
            None
        }
        fn inference_enum_type_by_file(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn inference_const_type(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn inference_const_type_alias(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn inference_const_value(&self, _: (FileId, Spur)) -> Option<i128> {
            None
        }
        fn inference_const_function_alias(&self, _: (FileId, Spur)) -> Option<Spur> {
            None
        }
        fn inference_module_binding_type(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn inference_module_file_id(&self, _: ModuleId) -> Option<FileId> {
            None
        }
        fn inference_function_by_file(&self, _: (FileId, Spur)) -> Option<Spur> {
            None
        }
    }

    #[test]
    fn owned_read_host_exercises_the_same_adapter_requests_as_sema() {
        use crate::inference::LazyInferenceFacts;
        use crate::sema::aggregate_resolution::EpochFacts as AggregateFactsView;
        use crate::sema::body_endpoint::EpochFacts as EndpointFactsView;
        use crate::sema::call_resolution::EpochFacts as CallFactsView;

        let interner = ThreadedRodeo::new();
        let symbol = interner.get_or_intern("body");
        let rir = rue_rir::Rir::new();
        let sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
        let owned = OwnedReadHost {
            ownership: OwnedHostSentinel { tag: 1 },
            label: Box::from("body"),
            symbol,
            path: Box::from("synthetic/0.rue"),
        };
        assert_eq!(owned.ownership.tag(), 1);

        let epoch_endpoint = EndpointFactsView::new(&sema);
        let owned_endpoint = EndpointFactsView::new(&owned);
        assert_eq!(
            owned_endpoint.name_symbol("body"),
            epoch_endpoint.name_symbol("body")
        );

        let epoch_call = CallFactsView::new(&sema);
        let owned_call = CallFactsView::new(&owned);
        assert_eq!(
            owned_call.source_function_name(symbol),
            epoch_call.source_function_name(symbol)
        );
        assert_eq!(
            owned_call.function_contains(symbol),
            epoch_call.function_contains(symbol)
        );

        let epoch_aggregate = AggregateFactsView::new(&sema);
        let owned_aggregate = AggregateFactsView::new(&owned);
        assert_eq!(
            owned_aggregate.file_path(FileId::DEFAULT),
            epoch_aggregate.file_path(FileId::DEFAULT)
        );

        let epoch_context = InferenceContext::new(&sema);
        let owned_context = InferenceContext::new(&owned);
        let epoch_inference = HostInferenceFacts::new(&epoch_context, &sema);
        let owned_inference = HostInferenceFacts::new(&owned_context, &owned);
        assert!(owned_inference.func_sig(symbol).is_none());
        assert!(epoch_inference.func_sig(symbol).is_none());
        assert_eq!(
            owned_inference.function_by_file((FileId::DEFAULT, symbol)),
            epoch_inference.function_by_file((FileId::DEFAULT, symbol)),
        );
    }
}
