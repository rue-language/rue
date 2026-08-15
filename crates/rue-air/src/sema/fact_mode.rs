//! Host capabilities for canonical body analysis.
//!
//! The trait deliberately describes only the facts an analyzer host can answer.
//! It does not name the epoch, `Sema`, declaration phases, or any particular
//! storage strategy. The provider body host supplies these facts to executable
//! body analysis.

use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_rir::InstRef;

    use super::*;
    use crate::inference::{FunctionSig, MethodSig};
    use crate::sema::aggregate_resolution::AggregateModuleFact;
    use crate::sema::anon_structs::IssuedAnonymousNominalKey;
    use crate::sema::info::{
        ConstInfo, FunctionCallInfo, FunctionInfo, MethodCallInfo, MethodInfo,
    };
    use crate::sema::{HostInferenceFacts, InferenceContext, PreviewFeatures, Sema};
    use crate::types::{EnumId, ModuleDef, ModuleId, StructId};
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

    impl BodyEndpointProvider for OwnedReadHost {
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

    impl CallResolutionFacts for OwnedReadHost {
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

    impl AggregateFacts for OwnedReadHost {
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
    fn owned_read_host_exercises_the_same_fact_contracts_as_sema() {
        use crate::inference::LazyInferenceFacts;

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

        assert_eq!(
            owned.endpoint_name_symbol("body"),
            sema.endpoint_name_symbol("body")
        );

        assert_eq!(
            owned.call_source_function_name(symbol),
            sema.call_source_function_name(symbol)
        );
        assert_eq!(
            owned.call_function_contains(symbol),
            sema.call_function_contains(symbol)
        );

        assert_eq!(
            owned.aggregate_file_path(FileId::DEFAULT),
            sema.aggregate_file_path(FileId::DEFAULT)
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
