//! Host capabilities for canonical one-body semantic analysis.
//!
//! The trait deliberately describes only the facts an analyzer host can answer.
//! It does not name the epoch, `Sema`, declaration phases, or any particular
//! storage strategy. The canonical `Sema` implements it below; a future owned
//! body host can implement the identical contract without borrowing an epoch.

use std::collections::HashMap;

use lasso::Spur;
use rue_span::{FileId, Span};

use super::aggregate_resolution::{AggregateFacts, EpochFacts as EpochAggregateFacts};
use super::body_endpoint::{BodyEndpointProvider, EpochFacts as EpochEndpointFacts};
use super::call_resolution::{CallResolutionFacts, EpochFacts as EpochCallFacts};
use super::typeck::TypeRootAuthority;
use super::{ConstValue, DeclarationPhase, InferenceContext, Sema, SemaInferenceFacts};
use crate::inference::LazyInferenceFacts;
use crate::types::{ArrayLen, ModuleId, Type};

/// Exact input for resolving one type-syntax fragment.
pub(crate) struct TypeSyntaxRequest<'a> {
    pub(crate) syntax: &'a str,
    pub(crate) root_file: FileId,
    pub(crate) root_authority: TypeRootAuthority,
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

/// Exact input for validating a deferred type-position parameter.
pub(crate) struct DeferredTypeRequest<'a> {
    pub(crate) type_name: String,
    pub(crate) type_params: &'a [Spur],
    pub(crate) value_params: &'a [Spur],
    pub(crate) value_param_type_syms: &'a [(Spur, Spur)],
    pub(crate) span: Span,
}

/// Exact input for validating a deferred value-position parameter.
pub(crate) struct DeferredValueRequest<'a> {
    pub(crate) value_name: &'a str,
    pub(crate) type_params: &'a [Spur],
    pub(crate) value_params: &'a [Spur],
    pub(crate) value_param_type_syms: &'a [(Spur, Spur)],
    pub(crate) expected: Option<Type>,
    pub(crate) contract: Option<(Spur, Spur)>,
    pub(crate) require_integer: bool,
    pub(crate) span: Span,
}

type TypeSyntaxResult = Result<
    Type,
    crate::SemanticTypeSyntaxError<std::convert::Infallible, rue_error::CompileError, FileId, Spur>,
>;

/// Host capability consumed by the canonical one-body engine.
///
/// Associated fact facades return owned or short-lived read-only values; the
/// mutable operations take a request object so the contract stays independent
/// of the current analyzer representation.
pub(crate) trait BodyAnalysisHost: Sized {
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
    fn resolve_type_module_prefix(
        &mut self,
        request: ModulePrefixRequest<'_>,
    ) -> rue_error::CompileResult<(ModuleId, Option<FileId>, String)>;
    fn resolve_array_length(
        &mut self,
        request: ArrayLengthRequest<'_>,
    ) -> rue_error::CompileResult<u64>;
    fn validate_deferred_type(
        &mut self,
        request: DeferredTypeRequest<'_>,
    ) -> rue_error::CompileResult<Option<Type>>;
    fn validate_deferred_value(
        &mut self,
        request: DeferredValueRequest<'_>,
    ) -> rue_error::CompileResult<Option<ConstValue>>;
}

/// The canonical epoch host. This is the only location that names the epoch
/// adapters; the host contract above remains representation-independent.
impl<'source, D: DeclarationPhase> BodyAnalysisHost for Sema<'source, D> {
    type EndpointFacts<'a>
        = EpochEndpointFacts<'a, 'source, D>
    where
        Self: 'a;

    fn endpoint_facts(&self) -> Self::EndpointFacts<'_> {
        EpochEndpointFacts::new(self)
    }

    type CallFacts<'a>
        = EpochCallFacts<'a, 'source, D>
    where
        Self: 'a;

    fn call_facts(&self) -> Self::CallFacts<'_> {
        EpochCallFacts::new(self)
    }

    type AggregateFacts<'a>
        = EpochAggregateFacts<'a, 'source, D>
    where
        Self: 'a;

    fn aggregate_facts(&self) -> Self::AggregateFacts<'_> {
        EpochAggregateFacts::new(self)
    }

    type InferenceFacts<'a>
        = SemaInferenceFacts<'a, 'source, D>
    where
        Self: 'a;

    fn inference_facts<'a>(&'a self, ctx: &'a InferenceContext) -> Self::InferenceFacts<'a> {
        SemaInferenceFacts::new(ctx, self)
    }

    fn resolve_type_syntax(&mut self, request: TypeSyntaxRequest<'_>) -> TypeSyntaxResult {
        self.resolve_type_syntax_with_epoch_facts(
            request.syntax,
            request.root_file,
            request.root_authority,
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

    fn validate_deferred_type(
        &mut self,
        request: DeferredTypeRequest<'_>,
    ) -> rue_error::CompileResult<Option<Type>> {
        self.validate_deferred_type_position_with_epoch_facts(
            request.type_name,
            request.type_params,
            request.value_params,
            request.value_param_type_syms,
            request.span,
        )
    }

    fn validate_deferred_value(
        &mut self,
        request: DeferredValueRequest<'_>,
    ) -> rue_error::CompileResult<Option<ConstValue>> {
        self.validate_deferred_value_position_with_epoch_facts(
            request.value_name,
            request.type_params,
            request.value_params,
            request.value_param_type_syms,
            request.expected,
            request.contract,
            request.require_integer,
            request.span,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use rue_rir::InstRef;

    use super::*;
    use crate::inference::{FunctionSig, MethodSig};
    use crate::sema::anon_structs::IssuedAnonymousNominalKey;
    use crate::sema::declaration_index::RirDestructorDeclaration;
    use crate::sema::info::{ConstInfo, FunctionInfo, MethodInfo};
    use crate::types::{EnumId, ModuleDef, StructId};
    use crate::{
        SemanticDefinitionEndpoint, SemanticDefinitionToken, SemanticModuleEndpoint,
        SemanticModuleToken,
    };

    /// A deliberately owned, non-`Clone` host shape. This is compile-only: it
    /// proves the capability does not need an epoch handle or a cloneable host.
    struct OwnedStubHost {
        _owned_label: Box<str>,
    }

    /// A zero-sized sentinel used for every read-only fact facade.
    struct StubFacts;

    impl BodyEndpointProvider for StubFacts {
        fn name_symbol(&self, _: &str) -> Option<Spur> {
            None
        }
        fn definition_endpoint(
            &self,
            _: SemanticDefinitionToken,
        ) -> Option<SemanticDefinitionEndpoint> {
            None
        }
        fn module_endpoint(&self, _: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
            None
        }
        fn function_by_file_name(&self, _: FileId, _: Spur) -> Option<Spur> {
            None
        }
        fn struct_by_file_name(&self, _: FileId, _: Spur) -> Option<StructId> {
            None
        }
        fn enum_by_file_name(&self, _: FileId, _: Spur) -> Option<EnumId> {
            None
        }
        fn builtin_or_generated_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn generated_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn builtin_enum(&self, _: Spur) -> Option<EnumId> {
            None
        }
        fn anon_struct(&self, _: &IssuedAnonymousNominalKey) -> Option<StructId> {
            None
        }
        fn anon_enum(&self, _: &IssuedAnonymousNominalKey) -> Option<EnumId> {
            None
        }
        fn is_builtin_or_generated_struct(&self, _: Spur) -> bool {
            false
        }
        fn is_builtin_enum(&self, _: Spur) -> bool {
            false
        }
        fn function_info(&self, _: Spur) -> Option<FunctionInfo> {
            None
        }
        fn method_info(&self, _: StructId, _: Spur) -> Option<MethodInfo> {
            None
        }
        fn source_function_name(&self, name: Spur) -> Spur {
            name
        }
        fn first_free_function(&self, _: Spur, _: FileId) -> Option<InstRef> {
            None
        }
        fn named_method_declaration(&self, _: FileId, _: Spur, _: Spur) -> Option<InstRef> {
            None
        }
        fn destructor(&self, _: u32, _: Spur) -> Option<RirDestructorDeclaration> {
            None
        }
        fn module_id_for_file(&self, _: u32) -> Option<ModuleId> {
            None
        }
        fn intern_array(&self, _: Type, _: u64) -> Option<Type> {
            None
        }
        fn intern_ptr_const(&self, _: Type) -> Option<Type> {
            None
        }
        fn intern_ptr_mut(&self, _: Type) -> Option<Type> {
            None
        }
    }

    impl CallResolutionFacts for StubFacts {
        fn function_info(&self, _: Spur) -> Option<FunctionInfo> {
            None
        }
        fn function_contains(&self, _: Spur) -> bool {
            false
        }
        fn source_function_name(&self, name: Spur) -> Spur {
            name
        }
        fn resolve_function_name_local(&self, _: Spur, _: FileId) -> Option<Spur> {
            None
        }
        fn resolve_const_info_in_file(&self, _: Spur, _: FileId) -> Option<ConstInfo> {
            None
        }
        fn value_const(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn module_binding(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn method_info(&self, _: StructId, _: Spur) -> Option<MethodInfo> {
            None
        }
        fn named_method_info(&self, _: StructId, _: Spur) -> Option<MethodInfo> {
            None
        }
        fn named_method_by_callable_symbol(&self, _: Spur) -> Option<(StructId, Spur, MethodInfo)> {
            None
        }
        fn named_method_declaration(&self, _: FileId, _: Spur, _: Spur) -> Option<InstRef> {
            None
        }
        fn module_def(&self, _: ModuleId) -> ModuleDef {
            unreachable!("compile-only stub")
        }
    }

    impl AggregateFacts for StubFacts {
        fn value_const(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn module_binding(&self, _: FileId, _: Spur) -> Option<ConstInfo> {
            None
        }
        fn struct_in_file(&self, _: FileId, _: Spur) -> Option<StructId> {
            None
        }
        fn enum_in_file(&self, _: FileId, _: Spur) -> Option<EnumId> {
            None
        }
        fn builtin_struct(&self, _: Spur) -> Option<StructId> {
            None
        }
        fn builtin_enum(&self, _: Spur) -> Option<EnumId> {
            None
        }
        fn module(&self, _: ModuleId) -> super::super::aggregate_resolution::AggregateModuleFact {
            unreachable!("compile-only stub")
        }
        fn file_path(&self, _: FileId) -> Option<&str> {
            None
        }
        fn source_path(&self, _: Span) -> Option<&str> {
            None
        }
    }

    impl LazyInferenceFacts for StubFacts {
        fn func_sig(&self, _: Spur) -> Option<Rc<FunctionSig>> {
            None
        }
        fn method_sig(&self, _: (StructId, Spur)) -> Option<Rc<MethodSig>> {
            None
        }
        fn builtin_struct_type(&self, _: Spur) -> Option<Type> {
            None
        }
        fn struct_type_by_file(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn builtin_enum_type(&self, _: Spur) -> Option<Type> {
            None
        }
        fn enum_type_by_file(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn const_type(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn const_type_alias(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn const_value(&self, _: (FileId, Spur)) -> Option<i128> {
            None
        }
        fn const_function_alias(&self, _: (FileId, Spur)) -> Option<Spur> {
            None
        }
        fn module_binding_type(&self, _: (FileId, Spur)) -> Option<Type> {
            None
        }
        fn module_file_id(&self, _: ModuleId) -> Option<FileId> {
            None
        }
        fn function_by_file(&self, _: (FileId, Spur)) -> Option<Spur> {
            None
        }
    }

    impl BodyAnalysisHost for OwnedStubHost {
        type EndpointFacts<'a>
            = StubFacts
        where
            Self: 'a;
        fn endpoint_facts(&self) -> Self::EndpointFacts<'_> {
            StubFacts
        }

        type CallFacts<'a>
            = StubFacts
        where
            Self: 'a;
        fn call_facts(&self) -> Self::CallFacts<'_> {
            StubFacts
        }

        type AggregateFacts<'a>
            = StubFacts
        where
            Self: 'a;
        fn aggregate_facts(&self) -> Self::AggregateFacts<'_> {
            StubFacts
        }

        type InferenceFacts<'a>
            = StubFacts
        where
            Self: 'a;
        fn inference_facts<'a>(&'a self, _: &'a InferenceContext) -> Self::InferenceFacts<'a> {
            StubFacts
        }

        fn resolve_type_syntax(&mut self, _: TypeSyntaxRequest<'_>) -> TypeSyntaxResult {
            unreachable!("compile-only stub")
        }
        fn resolve_type_module_prefix(
            &mut self,
            _: ModulePrefixRequest<'_>,
        ) -> rue_error::CompileResult<(ModuleId, Option<FileId>, String)> {
            unreachable!("compile-only stub")
        }
        fn resolve_array_length(
            &mut self,
            _: ArrayLengthRequest<'_>,
        ) -> rue_error::CompileResult<u64> {
            unreachable!("compile-only stub")
        }
        fn validate_deferred_type(
            &mut self,
            _: DeferredTypeRequest<'_>,
        ) -> rue_error::CompileResult<Option<Type>> {
            unreachable!("compile-only stub")
        }
        fn validate_deferred_value(
            &mut self,
            _: DeferredValueRequest<'_>,
        ) -> rue_error::CompileResult<Option<ConstValue>> {
            unreachable!("compile-only stub")
        }
    }

    fn accepts_host<H: BodyAnalysisHost>(host: H) -> H {
        host
    }

    fn assert_owned_host(_: OwnedStubHost) {}

    #[test]
    fn owned_non_clone_host_compiles_by_value() {
        let host = accepts_host(OwnedStubHost {
            _owned_label: Box::from("owned host"),
        });
        assert_owned_host(host);
    }
}
