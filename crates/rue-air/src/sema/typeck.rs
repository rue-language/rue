//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind, PreviewFeature};
use rue_span::{FileId, Span};

use super::{DeclarationPhase, Sema};

/// Frame-displacement budget a single object's slots must address: `i32::MAX`
/// bytes, matching the codegen frame-offset (disp32) addressing range (spec
/// C.4:3, RUE-561). This is the derivation basis for [`MAX_TYPE_SLOTS`], not a
/// limit that is checked on its own: nothing compares a byte total against it.
pub(crate) const MAX_TYPE_SIZE_BYTES: u64 = i32::MAX as u64;
/// Maximum ABI slot count for a single object: [`MAX_TYPE_SIZE_BYTES`] divided
/// by the 8-byte slot width, i.e. 268,435,455 slots. This — not a byte total —
/// is the limit E0906 enforces and names, because a layout spends one slot per
/// scalar, struct field, and array element whatever the element's own width
/// (spec C.4:2, RUE-1272). A `[i8; N]` therefore stops at this many *elements*,
/// well under `MAX_TYPE_SIZE_BYTES` bytes. Types past it are rejected with
/// E0906 rather than wrapping the slot arithmetic, per the graceful-failure
/// policy in spec C.1:2.
pub(crate) const MAX_TYPE_SLOTS: u64 = MAX_TYPE_SIZE_BYTES / 8;
use super::info::FunctionInfo;
use crate::inference::InferType;
use crate::sema::ConstValue;
use crate::types::{ArrayLen, ArrayTypeId, StructId, Type, TypeKind};

const COMPTIME_PARAMETER_INDEX_THRESHOLD: usize = 8;

/// Membership and declared-type facts shared by deferred signature walkers.
///
/// Most Rue generics have only a handful of compile-time parameters, so the
/// small path scans the parser-owned slices without allocating. Larger
/// signatures pay once to build indexes that every recursive syntax walk and
/// deferred type resolution can reuse.
pub(crate) struct DeferredParameterFacts<'a> {
    type_params: &'a [Spur],
    value_params: &'a [Spur],
    value_param_type_syntax: &'a [(Spur, rue_rir::RirTypeSyntaxRef)],
    type_param_index: Option<HashSet<Spur>>,
    value_param_index: Option<HashSet<Spur>>,
    value_param_type_index: Option<HashMap<Spur, rue_rir::RirTypeSyntaxRef>>,
}

impl<'a> DeferredParameterFacts<'a> {
    pub(crate) fn new(
        type_params: &'a [Spur],
        value_params: &'a [Spur],
        value_param_type_syntax: &'a [(Spur, rue_rir::RirTypeSyntaxRef)],
    ) -> Self {
        let type_param_index = (type_params.len() > COMPTIME_PARAMETER_INDEX_THRESHOLD)
            .then(|| type_params.iter().copied().collect());
        let value_param_index = (value_params.len() > COMPTIME_PARAMETER_INDEX_THRESHOLD)
            .then(|| value_params.iter().copied().collect());
        let value_param_type_index =
            (value_param_type_syntax.len() > COMPTIME_PARAMETER_INDEX_THRESHOLD).then(|| {
                let mut index = HashMap::with_capacity(value_param_type_syntax.len());
                for &(name, type_syntax) in value_param_type_syntax {
                    // Preserve the slice path's first-match behavior if invalid
                    // duplicate parameters reach deferred validation.
                    index.entry(name).or_insert(type_syntax);
                }
                index
            });
        Self {
            type_params,
            value_params,
            value_param_type_syntax,
            type_param_index,
            value_param_index,
            value_param_type_index,
        }
    }

    pub(crate) fn contains_type(&self, name: Spur) -> bool {
        self.type_param_index.as_ref().map_or_else(
            || self.type_params.contains(&name),
            |index| index.contains(&name),
        )
    }

    pub(crate) fn contains_value(&self, name: Spur) -> bool {
        self.value_param_index.as_ref().map_or_else(
            || self.value_params.contains(&name),
            |index| index.contains(&name),
        )
    }

    fn value_type_syntax(&self, name: Spur) -> Option<rue_rir::RirTypeSyntaxRef> {
        self.value_param_type_index.as_ref().map_or_else(
            || {
                self.value_param_type_syntax
                    .iter()
                    .find_map(|(candidate, syntax)| (*candidate == name).then_some(*syntax))
            },
            |index| index.get(&name).copied(),
        )
    }
}

/// The narrow semantic surface consumed by the canonical type-syntax
/// evaluator.  This deliberately owns neither a declaration epoch nor a body
/// analysis state: callers provide the host that owns the current facts.
pub(super) trait TypeSyntaxHost {
    fn type_syntax_symbol(&mut self, name: &str) -> Spur;
    fn existing_type_syntax_symbol(&self, name: &str) -> Option<Spur>;
    fn type_syntax_module_binding(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>;
    fn type_syntax_module_display_name(
        &self,
        module: crate::types::ModuleId,
    ) -> std::sync::Arc<str>;
    fn type_syntax_accessing_domain(
        &self,
        authority: TypeRootAuthority,
    ) -> crate::SemanticVisibilityDomain;
    fn type_syntax_named_type(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
        kind: TypeSyntaxNamedKind,
    ) -> CompileResult<Option<crate::SemanticTypeFact<Type, FileId>>>;
    fn type_syntax_make_str(&mut self, span: Span) -> CompileResult<Type>;
    fn type_syntax_make_array(
        &mut self,
        element: Type,
        length: u64,
        span: Span,
    ) -> CompileResult<Type>;
    fn type_syntax_make_ptr_const(&mut self, pointee: Type, span: Span) -> CompileResult<Type>;
    fn type_syntax_make_ptr_mut(&mut self, pointee: Type, span: Span) -> CompileResult<Type>;
    fn type_syntax_require_slices(&mut self, span: Span) -> CompileResult<()>;
    fn type_syntax_make_slice(
        &mut self,
        syntax: &str,
        element: Type,
        span: Span,
    ) -> CompileResult<Type>;
    fn type_syntax_make_fixed_str(&mut self, capacity: u64, span: Span) -> CompileResult<Type>;
    fn type_syntax_record_builtin_call(&mut self);
    fn type_syntax_constructor(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>>;
    fn type_syntax_reduce_constructor(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
        span: Span,
    ) -> CompileResult<Option<ConstValue>>;
    fn type_syntax_value_const(&self, file: FileId, name: Spur) -> Option<super::info::ConstInfo>;
    fn type_syntax_recover_const(
        &mut self,
        file: FileId,
        name: Spur,
    ) -> CompileResult<Option<ConstValue>>;
    fn type_syntax_record_named_const_dependency(&mut self, file: FileId, name: String);
    fn type_syntax_out_of_scope_const_hint(&self, name: Spur, exclude: FileId) -> String;
    fn type_syntax_dependencies(
        &self,
        ty: Type,
    ) -> Vec<(FileId, String, super::DeclarationTypeDependencyTargetKind)>;
    fn type_syntax_flush_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    );
}

/// Additional host capabilities used only while validating a declaration type
/// whose generic substitutions are not available yet. Keeping these off the
/// ordinary resolver contract lets every body-analysis host share the one
/// structured resolver without implementing declaration-epoch machinery it
/// cannot use.
pub(super) trait DeferredTypeSyntaxHost: TypeSyntaxHost {
    fn type_syntax_comptime_value_failure(
        &self,
        failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
        span: Span,
    ) -> CompileError;
    fn type_syntax_function_info(&self, key: Spur) -> FunctionInfo;
    fn type_syntax_parameter_types(&self, function: FunctionInfo) -> Vec<Type>;
    fn type_syntax_parameter_type_syntax(
        &self,
        function: FunctionInfo,
        index: usize,
    ) -> rue_rir::RirTypeSyntaxRef;
    fn type_syntax_named_symbol(&self, syntax: rue_rir::RirTypeSyntaxRef) -> Option<Spur>;
    fn type_syntax_signature_substitutions_are_ready(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_params: &[Spur],
        value_params: &[Spur],
        type_substitutions: &HashMap<Spur, Type>,
        value_substitutions: &HashMap<Spur, ConstValue>,
    ) -> bool;
    fn type_syntax_resolve_substituted_parameter_type(
        &mut self,
        function: &FunctionInfo,
        index: usize,
        declared_type: Type,
        type_substitutions: &HashMap<Spur, Type>,
        value_substitutions: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type>;
    fn type_syntax_type_mentions_type_param(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        parameters: &DeferredParameterFacts<'_>,
    ) -> bool;
    fn type_syntax_type_mentions_value_param(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        parameters: &DeferredParameterFacts<'_>,
    ) -> bool;
    fn type_syntax_resolve_structured_type(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
    ) -> CompileResult<Type>;
    fn type_syntax_type_display_name(&self, ty: Type) -> String;
    fn type_syntax_validate_deferred_value(
        &self,
        concrete: Option<ConstValue>,
        found: Option<Type>,
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        value_name: &str,
        span: Span,
    ) -> CompileResult<()>;
}

#[derive(Clone, Copy)]
pub(super) enum TypeSyntaxNamedKind {
    Struct,
    Enum,
    Alias,
}

type ObservedTypeDependency = (FileId, String, super::DeclarationTypeDependencyTargetKind);

pub(super) struct TypeSyntaxProvider<'host, 'c, H: TypeSyntaxHost> {
    host: &'host mut H,
    span: Span,
    root_authority: TypeRootAuthority,
    resolution_context: SemaTypeResolutionContext,
    type_substitutions: Option<&'c HashMap<Spur, Type>>,
    value_substitutions: Option<&'c HashMap<Spur, ConstValue>>,
    // Preserve observation order for the host while using a lazy membership
    // index once a resolution grows beyond the allocation-free small case.
    observed_type_dependencies: Vec<ObservedTypeDependency>,
    observed_type_dependency_index: Option<HashSet<ObservedTypeDependency>>,
}

/// The lexical root a type-syntax resolution walks from.
///
/// A source name only means something relative to the file whose declarations
/// and imports are in scope: two sibling modules may each declare `Cell`, and
/// the referencing file decides which one an unqualified mention selects
/// (RUE-497). This type therefore has exactly one form — a known file — and no
/// scope-free "global"/speculative construction; RUE-1126 deleted the last one.
///
/// Do not reintroduce a variant that stands for "no scope". Synthetic and
/// builtin types stay reachable without one: [`Sema::resolve_builtin_struct_name`]
/// and [`Sema::resolve_builtin_enum_name`] are consulted from inside the scoped
/// path, so a builtin lookup never needs to bypass the file namespace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TypeRootAuthority {
    file: FileId,
}

impl TypeRootAuthority {
    /// Anchor resolution at `file`. This is the only constructor: a caller
    /// cannot ask for a lookup that has no lexical scope.
    pub(super) fn in_file(file: FileId) -> Self {
        Self { file }
    }

    /// The file whose source namespace this resolution reads.
    pub(super) fn file(self) -> FileId {
        self.file
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SemaTypeResolutionContext {
    Type,
    ArrayLength,
}

type SemaProviderResult<T> = crate::SemanticProviderResult<T, Infallible, CompileError>;

fn provider_failure<T>(result: CompileResult<T>) -> SemaProviderResult<T> {
    result.map_err(crate::SemanticProviderError::Failure)
}

impl<H: TypeSyntaxHost> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>
    for TypeSyntaxProvider<'_, '_, H>
{
    type Abort = Infallible;
    type Failure = CompileError;

    fn root_module_binding(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(
            self.host
                .type_syntax_module_binding(self.root_authority, None, symbol),
        )
    }

    fn module_binding(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        let name = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_module_binding(
            self.root_authority,
            Some(*module),
            name,
        ))
    }

    fn module_display_name(&self, module: &crate::types::ModuleId) -> std::sync::Arc<str> {
        self.host.type_syntax_module_display_name(*module)
    }

    fn accessing_domain(&self, _scope: &FileId) -> crate::SemanticVisibilityDomain {
        self.host.type_syntax_accessing_domain(self.root_authority)
    }
}

impl<H: TypeSyntaxHost>
    crate::SemanticTypeSyntaxProvider<
        FileId,
        crate::types::ModuleId,
        FileId,
        Spur,
        Spur,
        Type,
        ConstValue,
    > for TypeSyntaxProvider<'_, '_, H>
{
    fn substituted_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<Type>> {
        let Some(type_substitutions) = self.type_substitutions else {
            return Ok(None);
        };
        let symbol = self.host.type_syntax_symbol(name);
        Ok(type_substitutions.get(&symbol).copied())
    }

    fn primitive_type(&mut self, name: &str) -> SemaProviderResult<Option<Type>> {
        Ok(Type::from_primitive_name(name))
    }

    fn builtin_type(&mut self, _scope: &FileId, name: &str) -> SemaProviderResult<Option<Type>> {
        if name == "str" {
            provider_failure(self.host.type_syntax_make_str(self.span).map(Some))
        } else {
            Ok(None)
        }
    }

    fn root_struct_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Struct,
        ))
    }

    fn root_enum_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Enum,
        ))
    }

    fn root_type_alias(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Alias,
        ))
    }

    fn module_struct_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Struct,
        ))
    }

    fn module_enum_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Enum,
        ))
    }

    fn module_type_alias(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Alias,
        ))
    }

    fn observe_selected_named_type(
        &mut self,
        name: &str,
        kind: crate::SemanticTypeFactKind,
        fact: &crate::SemanticTypeFact<Type, FileId>,
    ) -> SemaProviderResult<()> {
        match kind {
            crate::SemanticTypeFactKind::Struct | crate::SemanticTypeFactKind::Enum => {
                self.observe_materialized_type_fact(fact.value);
            }
            crate::SemanticTypeFactKind::Constant => {
                self.observe_type_dependency(
                    fact.site,
                    name.to_string(),
                    super::DeclarationTypeDependencyTargetKind::ValueConst,
                );
            }
            crate::SemanticTypeFactKind::Function => {}
        }
        Ok(())
    }

    fn observe_materialized_type(&mut self, ty: &Type) -> SemaProviderResult<()> {
        self.observe_materialized_type_fact(*ty);
        Ok(())
    }

    fn allows_qualified_paths(&self, _scope: &FileId) -> bool {
        true
    }

    fn allows_qualified_comptime_call_head(
        &self,
        _scope: &FileId,
        expectation: crate::SemanticComptimeCallExpectation,
    ) -> bool {
        !(self.resolution_context == SemaTypeResolutionContext::ArrayLength
            && expectation == crate::SemanticComptimeCallExpectation::Value)
    }

    fn resolve_array_length(
        &mut self,
        scope: &FileId,
        length: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<Option<u64>> {
        let length = match length {
            crate::SemanticValueSyntax::Integer(value) => {
                let value = u64::try_from(value).map_err(|_| {
                    crate::SemanticProviderError::Failure(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!("array length must be non-negative, got {value}"),
                        },
                        self.span,
                    ))
                })?;
                ArrayLen::Literal(value)
            }
            crate::SemanticValueSyntax::Name(name) => ArrayLen::Named(name.to_owned()),
        };
        provider_failure(self.resolve_array_length_fact(*scope, &length)).map(Some)
    }

    fn array_length_from_value(
        &mut self,
        _scope: &FileId,
        value: &ConstValue,
    ) -> SemaProviderResult<Option<u64>> {
        match value {
            ConstValue::Integer(value) => u64::try_from(*value).map(Some).map_err(|_| {
                crate::SemanticProviderError::Failure(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!("array length must be non-negative, got {value}"),
                    },
                    self.span,
                ))
            }),
            _ => provider_failure(Err(CompileError::new(
                ErrorKind::InvalidArrayLength {
                    reason: "array length must be an integer".to_string(),
                },
                self.span,
            ))),
        }
    }

    fn array_type(&mut self, element: Type, length: Option<u64>) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_array(
            element,
            length.expect("concrete type resolution always resolves array lengths"),
            self.span,
        ))
    }

    fn ptr_const_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_ptr_const(pointee, self.span))
    }

    fn ptr_mut_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_ptr_mut(pointee, self.span))
    }

    fn preflight_slice(&mut self, _scope: &FileId, _syntax: &str) -> SemaProviderResult<()> {
        provider_failure(self.host.type_syntax_require_slices(self.span))
    }

    fn slice_type(
        &mut self,
        _scope: &FileId,
        syntax: &str,
        element: Type,
    ) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_slice(syntax, element, self.span))
    }

    fn builtin_type_call(
        &mut self,
        scope: &FileId,
        name: &str,
        arguments: &[crate::SemanticValueSyntax<'_>],
    ) -> SemaProviderResult<Option<Type>> {
        if name == "Str" {
            let capacity = match arguments {
                [crate::SemanticValueSyntax::Integer(value)] => {
                    u64::try_from(*value).map_err(|_| {
                        crate::SemanticProviderError::Failure(CompileError::new(
                            ErrorKind::InvalidArrayLength {
                                reason: format!("array length must be non-negative, got {value}"),
                            },
                            self.span,
                        ))
                    })?
                }
                [crate::SemanticValueSyntax::Name(argument)] => {
                    provider_failure(self.resolve_array_length_fact(
                        *scope,
                        &ArrayLen::Named((*argument).to_owned()),
                    ))?
                }
                _ => {
                    return provider_failure(Err(CompileError::new(
                        ErrorKind::UnknownType(format!("{name}(...)")),
                        self.span,
                    )));
                }
            };
            self.host.type_syntax_record_builtin_call();
            provider_failure(
                self.host
                    .type_syntax_make_fixed_str(capacity, self.span)
                    .map(Some),
            )
        } else {
            Ok(None)
        }
    }

    fn root_constructor(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(
            self.host
                .type_syntax_constructor(self.root_authority, None, symbol),
        )
    }

    fn module_constructor(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_constructor(
            self.root_authority,
            Some(*module),
            symbol,
        ))
    }

    fn resolve_value_argument(
        &mut self,
        scope: &FileId,
        constructor: &str,
        _head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        _parameter_index: usize,
        _type_arguments: &[(Spur, Type)],
        _value_arguments: &[(Spur, ConstValue)],
        syntax: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<ConstValue> {
        if let crate::SemanticValueSyntax::Integer(value) = syntax {
            return Ok(ConstValue::Integer(value));
        }
        match self.resolve_value_argument_fact(*scope, constructor, syntax) {
            Ok(value) => Ok(value),
            Err(error) if self.resolution_context == SemaTypeResolutionContext::ArrayLength => {
                let crate::SemanticValueSyntax::Name(name) = syntax else {
                    return provider_failure(Err(error));
                };
                provider_failure(
                    self.resolve_array_length_fact(*scope, &ArrayLen::Named(name.to_owned()))
                        .map(|value| ConstValue::Integer(value as i128)),
                )
            }
            Err(error) => provider_failure(Err(error)),
        }
    }

    fn reduce_comptime_call(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
    ) -> SemaProviderResult<Option<crate::SemanticComptimeCallResult<Type, ConstValue>>> {
        provider_failure(
            self.host
                .type_syntax_reduce_constructor(head, type_arguments, value_arguments, self.span)
                .map(|result| match (head.returns_type, result) {
                    (_, None) => None,
                    (true, Some(ConstValue::Type(ty))) => {
                        Some(crate::SemanticComptimeCallResult::Type(ty))
                    }
                    (false, Some(value)) => Some(crate::SemanticComptimeCallResult::Value(value)),
                    (true, Some(_)) => None,
                }),
        )
    }
}

impl<'s, 'c, H: TypeSyntaxHost> TypeSyntaxProvider<'s, 'c, H> {
    pub(super) fn new(
        host: &'s mut H,
        span: Span,
        root_authority: TypeRootAuthority,
        resolution_context: SemaTypeResolutionContext,
        type_substitutions: Option<&'c HashMap<Spur, Type>>,
        value_substitutions: Option<&'c HashMap<Spur, ConstValue>>,
    ) -> Self {
        Self {
            host,
            span,
            root_authority,
            resolution_context,
            type_substitutions,
            value_substitutions,
            observed_type_dependencies: Vec::new(),
            observed_type_dependency_index: None,
        }
    }

    pub(super) fn resolve_array_length_fact(
        &mut self,
        scope: FileId,
        length: &ArrayLen,
    ) -> CompileResult<u64> {
        let previous_context = self.resolution_context;
        self.resolution_context = SemaTypeResolutionContext::ArrayLength;
        let result = self.resolve_array_length_fact_inner(scope, length);
        self.resolution_context = previous_context;
        result
    }

    fn resolve_array_length_fact_inner(
        &mut self,
        _scope: FileId,
        length: &ArrayLen,
    ) -> CompileResult<u64> {
        let ArrayLen::Named(name) = length else {
            let ArrayLen::Literal(value) = length else {
                unreachable!()
            };
            return Ok(*value);
        };
        let symbol = self.host.type_syntax_symbol(name);
        let root_file = self.root_authority.file();
        let value = if let Some(value) = self
            .value_substitutions
            .and_then(|substitutions| substitutions.get(&symbol))
        {
            *value
        } else if let Some(info) = self.host.type_syntax_value_const(root_file, symbol) {
            self.host
                .type_syntax_record_named_const_dependency(info.span.file_id, name.to_owned());
            info.value
        } else if let Some(value) = self.host.type_syntax_recover_const(root_file, symbol)? {
            self.host
                .type_syntax_record_named_const_dependency(root_file, name.to_owned());
            value
        } else {
            let hint = self
                .host
                .type_syntax_out_of_scope_const_hint(symbol, root_file);
            return Err(self.invalid_array_length(format!(
                "'{name}' is not a compile-time constant; array lengths must be an integer literal, a `const`, or a `comptime` value parameter{hint}"
            )));
        };

        match value.as_int_value() {
            Some(value) if value >= 0 => u64::try_from(value).map_err(|_| {
                self.invalid_array_length(format!("array length '{name}' ({value}) is too large"))
            }),
            Some(value) => {
                Err(self
                    .invalid_array_length(format!("array length '{name}' is negative ({value})")))
            }
            None => {
                Err(self.invalid_array_length(format!("array length '{name}' is not an integer")))
            }
        }
    }

    fn invalid_array_length(&self, reason: String) -> CompileError {
        CompileError::new(ErrorKind::InvalidArrayLength { reason }, self.span)
    }

    fn observe_materialized_type_fact(&mut self, ty: Type) {
        for (file, name, kind) in self.host.type_syntax_dependencies(ty) {
            self.observe_type_dependency(file, name, kind);
        }
    }

    pub(super) fn flush_observed_type_dependencies(&mut self) {
        if let Some(index) = &mut self.observed_type_dependency_index {
            index.clear();
        }
        for (file, name, kind) in std::mem::take(&mut self.observed_type_dependencies) {
            self.host.type_syntax_flush_dependency(file, name, kind);
        }
    }

    fn observe_type_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        const LINEAR_ADMISSION_LIMIT: usize = 8;

        let dependency = (file, name, kind);
        if let Some(index) = &mut self.observed_type_dependency_index {
            if index.insert(dependency.clone()) {
                self.observed_type_dependencies.push(dependency);
            }
            return;
        }
        if !self.observed_type_dependencies.contains(&dependency) {
            self.observed_type_dependencies.push(dependency);
            if self.observed_type_dependencies.len() == LINEAR_ADMISSION_LIMIT {
                self.observed_type_dependency_index =
                    Some(self.observed_type_dependencies.iter().cloned().collect());
            }
        }
    }

    fn resolve_value_argument_fact(
        &mut self,
        _scope: FileId,
        constructor: &str,
        syntax: crate::SemanticValueSyntax<'_>,
    ) -> CompileResult<ConstValue> {
        let crate::SemanticValueSyntax::Name(text) = syntax else {
            let crate::SemanticValueSyntax::Integer(value) = syntax else {
                unreachable!()
            };
            return Ok(ConstValue::Integer(value));
        };
        if text == "true" {
            return Ok(ConstValue::Bool(true));
        }
        if text == "false" {
            return Ok(ConstValue::Bool(false));
        }
        let symbol = self.host.type_syntax_symbol(text);
        if let Some(value_substitutions) = self.value_substitutions
            && let Some(value) = value_substitutions.get(&symbol)
        {
            return Ok(*value);
        }
        if let Some(type_substitutions) = self.type_substitutions
            && let Some(ty) = type_substitutions.get(&symbol)
        {
            return Ok(ConstValue::Type(*ty));
        }
        if let Some(info) = self
            .host
            .type_syntax_value_const(self.root_authority.file(), symbol)
        {
            return Ok(info.value);
        }
        Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "argument '{}' of type constructor '{}' must be a compile-time known value (an integer or bool literal, a comptime parameter, or a constant)",
                    text, constructor
                ),
            },
            self.span,
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeferredTypeResolution {
    Resolved(Type),
    Pending,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeferredValueResolution {
    Resolved(ConstValue),
    Pending,
}

pub(super) struct DeferredTypeSyntaxProvider<'s, 'c, 'p, H: DeferredTypeSyntaxHost> {
    inner: TypeSyntaxProvider<'s, 'c, H>,
    parameters: &'p DeferredParameterFacts<'c>,
}

impl<'s, 'c, 'p, H: DeferredTypeSyntaxHost> DeferredTypeSyntaxProvider<'s, 'c, 'p, H> {
    pub(super) fn new(
        host: &'s mut H,
        span: Span,
        parameters: &'p DeferredParameterFacts<'c>,
    ) -> Self {
        Self {
            inner: TypeSyntaxProvider::new(
                host,
                span,
                TypeRootAuthority::in_file(span.file_id),
                SemaTypeResolutionContext::Type,
                None,
                None,
            ),
            parameters,
        }
    }

    pub(super) fn flush_observed_type_dependencies(&mut self) {
        self.inner.flush_observed_type_dependencies();
    }

    fn deferred_argument_expected(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        parameter_index: usize,
        type_arguments: &[(Spur, DeferredTypeResolution)],
        value_arguments: &[(Spur, DeferredValueResolution)],
    ) -> CompileResult<Option<Type>> {
        let function = self.inner.host.type_syntax_function_info(head.key);
        let parameter_types = self.inner.host.type_syntax_parameter_types(function);
        let parameter_type_syntax = self
            .inner
            .host
            .type_syntax_parameter_type_syntax(function, parameter_index);
        let type_substitutions = type_arguments
            .iter()
            .filter_map(|(name, value)| match value {
                DeferredTypeResolution::Resolved(value) => Some((*name, *value)),
                DeferredTypeResolution::Pending => None,
            })
            .collect::<HashMap<_, _>>();
        let value_substitutions = value_arguments
            .iter()
            .filter_map(|(name, value)| match value {
                DeferredValueResolution::Resolved(value) => Some((*name, *value)),
                DeferredValueResolution::Pending => None,
            })
            .collect::<HashMap<_, _>>();
        if parameter_types[parameter_index] != Type::COMPTIME_TYPE {
            return Ok(Some(parameter_types[parameter_index]));
        }
        if let Some(parameter_type_symbol) = self
            .inner
            .host
            .type_syntax_named_symbol(parameter_type_syntax)
            && let Some(bound) = type_substitutions.get(&parameter_type_symbol)
        {
            return Ok(Some(*bound));
        }
        let type_params = head
            .parameters
            .iter()
            .filter_map(|parameter| parameter.is_type.then_some(parameter.name))
            .collect::<Vec<_>>();
        let value_params = head
            .parameters
            .iter()
            .filter_map(|parameter| (!parameter.is_type).then_some(parameter.name))
            .collect::<Vec<_>>();
        if self
            .inner
            .host
            .type_syntax_signature_substitutions_are_ready(
                parameter_type_syntax,
                &type_params,
                &value_params,
                &type_substitutions,
                &value_substitutions,
            )
        {
            return self
                .inner
                .host
                .type_syntax_resolve_substituted_parameter_type(
                    &function,
                    parameter_index,
                    parameter_types[parameter_index],
                    &type_substitutions,
                    &value_substitutions,
                )
                .map(Some);
        }
        Ok(None)
    }

    fn validate_value_position(
        &mut self,
        syntax: crate::SemanticValueSyntax<'_>,
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
    ) -> CompileResult<Option<ConstValue>> {
        let rendered;
        let value_name = match syntax {
            crate::SemanticValueSyntax::Integer(value) => {
                rendered = value.to_string();
                rendered.as_str()
            }
            crate::SemanticValueSyntax::Name(name) => name,
        };
        if let crate::SemanticValueSyntax::Name(value_name) = syntax
            && let Some(symbol) = self.inner.host.existing_type_syntax_symbol(value_name)
        {
            if self.parameters.contains_type(symbol) {
                if expected != Some(Type::COMPTIME_TYPE) && (expected.is_some() || require_integer)
                {
                    if let Some(expected) = expected {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: self.inner.host.type_syntax_type_display_name(expected),
                                found: self
                                    .inner
                                    .host
                                    .type_syntax_type_display_name(Type::COMPTIME_TYPE),
                            },
                            self.inner.span,
                        ));
                    }
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "compile-time type parameter '{}' is a type, not an integer value",
                                value_name
                            ),
                        },
                        self.inner.span,
                    ));
                }
                return Ok(None);
            }
            if self.parameters.contains_value(symbol) {
                let found = self
                    .parameters
                    .value_type_syntax(symbol)
                    .filter(|syntax| {
                        !self
                            .inner
                            .host
                            .type_syntax_type_mentions_type_param(*syntax, self.parameters)
                            && !self
                                .inner
                                .host
                                .type_syntax_type_mentions_value_param(*syntax, self.parameters)
                    })
                    .map(|syntax| {
                        self.inner
                            .host
                            .type_syntax_resolve_structured_type(syntax, self.inner.span)
                    })
                    .transpose()?;
                self.inner.host.type_syntax_validate_deferred_value(
                    None,
                    found,
                    expected,
                    contract,
                    require_integer,
                    value_name,
                    self.inner.span,
                )?;
                return Ok(None);
            }
        }

        let value = if let crate::SemanticValueSyntax::Integer(value) = syntax {
            ConstValue::Integer(value)
        } else {
            let crate::SemanticValueSyntax::Name(value_name) = syntax else {
                unreachable!()
            };
            let mut provider = TypeSyntaxProvider::new(
                self.inner.host,
                self.inner.span,
                TypeRootAuthority::in_file(self.inner.span.file_id),
                SemaTypeResolutionContext::Type,
                None,
                None,
            );
            let value = match provider.resolve_value_argument_fact(
                provider.span.file_id,
                "compile-time call",
                syntax,
            ) {
                Ok(value) => Ok(value),
                Err(_) if require_integer => provider
                    .resolve_array_length_fact(
                        provider.span.file_id,
                        &ArrayLen::Named(value_name.to_owned()),
                    )
                    .map(|length| ConstValue::Integer(length as i128)),
                Err(error) => Err(error),
            };
            provider.flush_observed_type_dependencies();
            value?
        };
        self.inner.host.type_syntax_validate_deferred_value(
            Some(value),
            Some(value.get_type()),
            expected,
            contract,
            require_integer,
            value_name,
            self.inner.span,
        )?;
        Ok(Some(value))
    }
}

impl<H: DeferredTypeSyntaxHost>
    crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>
    for DeferredTypeSyntaxProvider<'_, '_, '_, H>
{
    type Abort = Infallible;
    type Failure = CompileError;

    fn root_module_binding(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        self.inner.root_module_binding(scope, name)
    }

    fn module_binding(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        self.inner.module_binding(module, name)
    }

    fn module_display_name(&self, module: &crate::types::ModuleId) -> std::sync::Arc<str> {
        self.inner.module_display_name(module)
    }

    fn accessing_domain(&self, scope: &FileId) -> crate::SemanticVisibilityDomain {
        self.inner.accessing_domain(scope)
    }
}

impl<H: DeferredTypeSyntaxHost>
    crate::SemanticTypeSyntaxProvider<
        FileId,
        crate::types::ModuleId,
        FileId,
        Spur,
        Spur,
        DeferredTypeResolution,
        DeferredValueResolution,
    > for DeferredTypeSyntaxProvider<'_, '_, '_, H>
{
    fn substituted_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        let Some(symbol) = self.inner.host.existing_type_syntax_symbol(name) else {
            return Ok(None);
        };
        if self.parameters.contains_type(symbol) {
            return Ok(Some(DeferredTypeResolution::Pending));
        }
        if self.parameters.contains_value(symbol) {
            return provider_failure(Err(CompileError::new(
                ErrorKind::UnknownType(name.to_string()),
                self.inner.span,
            )));
        }
        Ok(None)
    }

    fn primitive_type(&mut self, name: &str) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .primitive_type(name)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn builtin_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .builtin_type(scope, name)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn root_struct_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_struct_type(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn root_enum_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_enum_type(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn root_type_alias(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_type_alias(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_struct_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_struct_type(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_enum_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_enum_type(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_type_alias(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_type_alias(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn observe_selected_named_type(
        &mut self,
        name: &str,
        kind: crate::SemanticTypeFactKind,
        fact: &crate::SemanticTypeFact<DeferredTypeResolution, FileId>,
    ) -> SemaProviderResult<()> {
        if let DeferredTypeResolution::Resolved(value) = fact.value {
            let resolved = crate::SemanticTypeFact {
                value,
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain.clone(),
                defining_file: fact.defining_file.clone(),
            };
            self.inner
                .observe_selected_named_type(name, kind, &resolved)?;
        }
        Ok(())
    }

    fn observe_materialized_type(&mut self, ty: &DeferredTypeResolution) -> SemaProviderResult<()> {
        if let DeferredTypeResolution::Resolved(ty) = ty {
            self.inner.observe_materialized_type(ty)?;
        }
        Ok(())
    }

    fn allows_qualified_paths(&self, scope: &FileId) -> bool {
        self.inner.allows_qualified_paths(scope)
    }

    fn allows_qualified_comptime_call_head(
        &self,
        scope: &FileId,
        expectation: crate::SemanticComptimeCallExpectation,
    ) -> bool {
        expectation != crate::SemanticComptimeCallExpectation::Value
            && self
                .inner
                .allows_qualified_comptime_call_head(scope, expectation)
    }

    fn resolve_array_length(
        &mut self,
        _scope: &FileId,
        length: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<Option<u64>> {
        if let crate::SemanticValueSyntax::Integer(length) = length {
            return u64::try_from(length).map(Some).map_err(|_| {
                crate::SemanticProviderError::Failure(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!("array length must be non-negative, got {length}"),
                    },
                    self.inner.span,
                ))
            });
        };
        let name = match length {
            crate::SemanticValueSyntax::Name(name) => name,
            crate::SemanticValueSyntax::Integer(_) => unreachable!(),
        };
        let value = provider_failure(self.validate_value_position(
            crate::SemanticValueSyntax::Name(name),
            None,
            None,
            true,
        ))?;
        match value {
            None => Ok(None),
            Some(ConstValue::Integer(value)) => u64::try_from(value).map(Some).map_err(|_| {
                crate::SemanticProviderError::Failure(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!("array length must be non-negative, got {value}"),
                    },
                    self.inner.span,
                ))
            }),
            Some(_) => provider_failure(Err(CompileError::new(
                ErrorKind::InvalidArrayLength {
                    reason: "array length must be an integer".to_string(),
                },
                self.inner.span,
            ))),
        }
    }

    fn array_length_from_value(
        &mut self,
        scope: &FileId,
        value: &DeferredValueResolution,
    ) -> SemaProviderResult<Option<u64>> {
        match value {
            DeferredValueResolution::Resolved(value) => {
                self.inner.array_length_from_value(scope, value)
            }
            DeferredValueResolution::Pending => Ok(None),
        }
    }

    fn array_type(
        &mut self,
        element: DeferredTypeResolution,
        length: Option<u64>,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match (element, length) {
            (DeferredTypeResolution::Resolved(element), Some(length)) => self
                .inner
                .array_type(element, Some(length))
                .map(DeferredTypeResolution::Resolved),
            _ => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn ptr_const_type(
        &mut self,
        pointee: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match pointee {
            DeferredTypeResolution::Resolved(pointee) => self
                .inner
                .ptr_const_type(pointee)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn ptr_mut_type(
        &mut self,
        pointee: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match pointee {
            DeferredTypeResolution::Resolved(pointee) => self
                .inner
                .ptr_mut_type(pointee)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn preflight_slice(&mut self, scope: &FileId, syntax: &str) -> SemaProviderResult<()> {
        self.inner.preflight_slice(scope, syntax)
    }

    fn slice_type(
        &mut self,
        scope: &FileId,
        syntax: &str,
        element: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match element {
            DeferredTypeResolution::Resolved(element) => self
                .inner
                .slice_type(scope, syntax, element)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn builtin_type_call(
        &mut self,
        scope: &FileId,
        name: &str,
        arguments: &[crate::SemanticValueSyntax<'_>],
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .builtin_type_call(scope, name, arguments)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn root_constructor(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        self.inner.root_constructor(scope, name)
    }

    fn module_constructor(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        self.inner.module_constructor(module, name)
    }

    fn resolve_value_argument(
        &mut self,
        _scope: &FileId,
        constructor: &str,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        parameter_index: usize,
        type_arguments: &[(Spur, DeferredTypeResolution)],
        value_arguments: &[(Spur, DeferredValueResolution)],
        syntax: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<DeferredValueResolution> {
        let expected = provider_failure(self.deferred_argument_expected(
            head,
            parameter_index,
            type_arguments,
            value_arguments,
        ))?;
        let contract = Some((
            self.inner.host.type_syntax_symbol(constructor),
            head.parameters[parameter_index].name,
        ));
        provider_failure(self.validate_value_position(syntax, expected, contract, false)).map(
            |value| match value {
                Some(value) => DeferredValueResolution::Resolved(value),
                None => DeferredValueResolution::Pending,
            },
        )
    }

    fn reduce_comptime_call(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, DeferredTypeResolution)],
        value_arguments: &[(Spur, DeferredValueResolution)],
    ) -> SemaProviderResult<
        Option<crate::SemanticComptimeCallResult<DeferredTypeResolution, DeferredValueResolution>>,
    > {
        if type_arguments
            .iter()
            .any(|(_, value)| *value == DeferredTypeResolution::Pending)
            || value_arguments
                .iter()
                .any(|(_, value)| *value == DeferredValueResolution::Pending)
        {
            return Ok(Some(if head.returns_type {
                crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Pending)
            } else {
                crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Pending)
            }));
        }
        let resolved_types = type_arguments
            .iter()
            .map(|(name, value)| match value {
                DeferredTypeResolution::Resolved(value) => (*name, *value),
                DeferredTypeResolution::Pending => unreachable!(),
            })
            .collect::<Vec<_>>();
        let resolved_values = value_arguments
            .iter()
            .map(|(name, value)| match value {
                DeferredValueResolution::Resolved(value) => (*name, *value),
                DeferredValueResolution::Pending => unreachable!(),
            })
            .collect::<Vec<_>>();
        self.inner
            .reduce_comptime_call(head, &resolved_types, &resolved_values)
            .map(|result| {
                result.map(|result| match result {
                    crate::SemanticComptimeCallResult::Type(value) => {
                        crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Resolved(
                            value,
                        ))
                    }
                    crate::SemanticComptimeCallResult::Value(value) => {
                        crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Resolved(
                            value,
                        ))
                    }
                })
            })
    }
}

fn module_path_compile_error(
    failure: crate::SemanticModulePathFailure<FileId>,
    span: Span,
) -> CompileError {
    match failure {
        crate::SemanticModulePathFailure::Empty => {
            CompileError::new(ErrorKind::UnknownType(String::new()), span)
        }
        crate::SemanticModulePathFailure::UnknownRoot { name } => {
            CompileError::new(ErrorKind::UnknownType(name.to_string()), span)
        }
        crate::SemanticModulePathFailure::UnknownMember { module, member, .. } => {
            CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: module.to_string(),
                    member_name: member.to_string(),
                },
                span,
            )
        }
        crate::SemanticModulePathFailure::PrivateMember {
            member,
            defining_file,
            ..
        } => private_qualified_item_error("constant", &member, &defining_file, span),
    }
}

pub(super) fn module_path_resolution_compile_error(
    failure: crate::SemanticResolutionError<
        Infallible,
        CompileError,
        crate::SemanticModulePathFailure<FileId>,
    >,
    span: Span,
) -> CompileError {
    match failure {
        crate::SemanticResolutionError::ProviderAbort(error) => match error {},
        crate::SemanticResolutionError::ProviderFailure(error) => error,
        crate::SemanticResolutionError::Semantic(failure) => {
            module_path_compile_error(failure, span)
        }
        crate::SemanticResolutionError::ComptimeCallTypeArgument { error, .. } => {
            module_path_resolution_compile_error(*error, span)
        }
    }
}

fn private_qualified_item_error(
    item_kind: &str,
    member: &str,
    defining_file: &str,
    span: Span,
) -> CompileError {
    CompileError::new(
        ErrorKind::PrivateUnqualifiedAccess(Box::new(
            rue_error::PrivateUnqualifiedAccessData {
                item_kind: item_kind.to_string(),
                name: member.to_string(),
                defining_file: defining_file.to_string(),
            },
        )),
        span,
    )
    .with_help(format!(
        "`{member}` is not marked `pub`; private items are only visible within their defining directory"
    ))
}

pub(super) fn semantic_type_syntax_compile_error(
    interner: &lasso::ThreadedRodeo,
    failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
    span: Span,
) -> CompileError {
    use crate::SemanticResolutionError as E;
    use crate::SemanticTypeSyntaxFailure as F;

    match failure {
        E::ProviderAbort(error) => match error {},
        E::ProviderFailure(error) => error,
        E::Semantic(F::Path(failure)) => module_path_compile_error(failure, span),
        E::Semantic(F::UnknownType { syntax }) => {
            CompileError::new(ErrorKind::UnknownType(syntax.to_string()), span)
        }
        E::Semantic(F::UnknownConstructor {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Type,
        }) => CompileError::new(ErrorKind::UnknownType(format!("{constructor}(...)")), span),
        E::Semantic(F::UnknownConstructor {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
        }) => CompileError::new(
            ErrorKind::InvalidArrayLength {
                reason: format!(
                    "'{constructor}' is not a function; array lengths must be an integer literal, a `const`, a `comptime` value parameter, or a call to a comptime function"
                ),
            },
            span,
        ),
        E::Semantic(F::UnknownModuleMember { module, member, .. }) => CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module.to_string(),
                member_name: member.to_string(),
            },
            span,
        ),
        E::Semantic(F::PrivateItem {
            kind,
            name,
            defining_file,
            ..
        }) => private_qualified_item_error(kind.diagnostic_name(), &name, &defining_file, span),
        E::Semantic(F::AmbiguousItem { name, .. }) => CompileError::new(
            ErrorKind::InternalError(format!(
                "type resolution produced an ambiguous item for '{}'",
                name
            )),
            span,
        ),
        E::Semantic(F::NotTypeConstructor { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "'{}' is not a type: only a function returning `type` (a type constructor) can be applied as a type here",
                    constructor
                ),
            },
            span,
        ),
        E::Semantic(F::TypeWhereValueExpected { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "'{}' returns `type` and cannot be used where a compile-time value is required",
                    constructor
                ),
            },
            span,
        ),
        E::Semantic(F::InvalidConstructorArity {
            constructor,
            expected,
            found,
            expectation: crate::SemanticComptimeCallExpectation::Type,
            ..
        })
        | E::Semantic(F::RuntimeConstructorParameter {
            constructor,
            expected,
            found,
            expectation: crate::SemanticComptimeCallExpectation::Type,
            ..
        }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "type constructor '{}' expects {} comptime type argument(s), but {} were provided",
                    constructor, expected, found
                ),
            },
            span,
        ),
        E::Semantic(F::InvalidConstructorArity {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
            ..
        })
        | E::Semantic(F::RuntimeConstructorParameter {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
            ..
        }) => CompileError::new(
            ErrorKind::InvalidArrayLength {
                reason: format!(
                    "array length call '{constructor}(...)' is not a compile-time constant; its callee must be a value-returning function whose parameters are all `comptime`"
                ),
            },
            span,
        ),
        E::Semantic(F::ValueWhereTypeExpected {
            constructor,
            argument,
            parameter,
            ..
        }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "argument '{}' of type constructor '{}' must be a type (this parameter is `comptime {}: type`)",
                    argument,
                    constructor,
                    interner.resolve(&parameter)
                ),
            },
            span,
        ),
        E::ComptimeCallTypeArgument { error, .. } => {
            semantic_type_syntax_compile_error(interner, *error, span)
        }
        E::Semantic(F::ConstructorDidNotReduce { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "the type constructor '{}' did not reduce to a concrete type at compile time",
                    constructor
                ),
            },
            span,
        ),
    }
}

impl<'source, D: DeclarationPhase> TypeSyntaxHost for Sema<'source, D> {
    fn type_syntax_symbol(&mut self, name: &str) -> Spur {
        self.interner.get_or_intern(name)
    }

    fn existing_type_syntax_symbol(&self, name: &str) -> Option<Spur> {
        self.interner.get(name)
    }

    fn type_syntax_module_binding(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>> {
        let file = match module {
            None => authority.file(),
            Some(module) => self.module_registry.get_def(module).file_id,
        };
        let Some(binding) = self.resolve_module_binding_in_file(file, name)? else {
            return Ok(None);
        };
        let Some(target) = binding.ty.as_module() else {
            return Ok(None);
        };
        let defining_file = self
            .get_file_path(binding.span.file_id)
            .unwrap_or("<unknown>");
        Ok(Some(crate::SemanticModuleBinding {
            target,
            site: binding.span.file_id,
            is_public: binding.is_pub,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                self.get_file_path(binding.span.file_id),
            ),
            defining_file: std::sync::Arc::from(defining_file),
        }))
    }

    fn type_syntax_module_display_name(
        &self,
        module: crate::types::ModuleId,
    ) -> std::sync::Arc<str> {
        std::sync::Arc::from(self.module_registry.get_def(module).import_path.as_str())
    }

    fn type_syntax_accessing_domain(
        &self,
        authority: TypeRootAuthority,
    ) -> crate::SemanticVisibilityDomain {
        crate::SemanticVisibilityDomain::from_file_path(self.get_file_path(authority.file()))
    }

    fn type_syntax_named_type(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
        kind: TypeSyntaxNamedKind,
    ) -> CompileResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let file = match module {
            None => authority.file(),
            Some(module) => self.module_registry.get_def(module).file_id,
        };
        let fact =
            |this: &Self, value: Type, site: FileId, is_public: bool| crate::SemanticTypeFact {
                value,
                site,
                is_public,
                defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                    this.get_file_path(site),
                ),
                defining_file: std::sync::Arc::from(
                    this.get_file_path(site).unwrap_or("<unknown>"),
                ),
            };
        match kind {
            TypeSyntaxNamedKind::Struct => {
                let id = self
                    .structs_by_file_name
                    .get(&(file, name))
                    .copied()
                    .or_else(|| {
                        (module.is_none())
                            .then(|| self.resolve_builtin_struct_name(name))
                            .flatten()
                    });
                let Some(id) = id else { return Ok(None) };
                let metadata = self
                    .type_pool
                    .struct_metadata(id)
                    .expect("type lookup names a registered struct identity");
                Ok(Some(fact(
                    self,
                    Type::new_struct(id),
                    metadata.file_id,
                    metadata.is_pub,
                )))
            }
            TypeSyntaxNamedKind::Enum => {
                let id = self
                    .enums_by_file_name
                    .get(&(file, name))
                    .copied()
                    .or_else(|| {
                        (module.is_none())
                            .then(|| self.resolve_builtin_enum_name(name))
                            .flatten()
                    });
                let Some(id) = id else { return Ok(None) };
                let metadata = self
                    .type_pool
                    .enum_metadata(id)
                    .expect("type lookup names a registered enum identity");
                Ok(Some(fact(
                    self,
                    Type::new_enum(id),
                    metadata.file_id,
                    metadata.is_pub,
                )))
            }
            TypeSyntaxNamedKind::Alias => {
                if self.value_const(file, name).is_none() {
                    D::resolve_indexed_const(self, name, file)?;
                }
                let Some(info) = self.value_const(file, name) else {
                    return Ok(None);
                };
                let ConstValue::Type(value) = info.value else {
                    return Ok(None);
                };
                Ok(Some(fact(self, value, info.span.file_id, info.is_pub)))
            }
        }
    }

    fn type_syntax_make_str(&mut self, span: Span) -> CompileResult<Type> {
        self.get_or_create_str_struct(span)
    }
    fn type_syntax_make_array(
        &mut self,
        element: Type,
        length: u64,
        _span: Span,
    ) -> CompileResult<Type> {
        Ok(Type::new_array(
            self.get_or_create_array_type(element, length),
        ))
    }
    fn type_syntax_make_ptr_const(&mut self, pointee: Type, _span: Span) -> CompileResult<Type> {
        Ok(Type::new_ptr_const(
            self.type_pool.intern_ptr_const_from_type(pointee),
        ))
    }
    fn type_syntax_make_ptr_mut(&mut self, pointee: Type, _span: Span) -> CompileResult<Type> {
        Ok(Type::new_ptr_mut(
            self.type_pool.intern_ptr_mut_from_type(pointee),
        ))
    }
    fn type_syntax_require_slices(&mut self, span: Span) -> CompileResult<()> {
        self.require_preview(PreviewFeature::Slices, "the slice type `[T]`", span)
    }
    fn type_syntax_make_slice(
        &mut self,
        syntax: &str,
        element: Type,
        span: Span,
    ) -> CompileResult<Type> {
        self.get_or_create_slice_struct_from_element(syntax, element, span)
    }
    fn type_syntax_make_fixed_str(&mut self, capacity: u64, span: Span) -> CompileResult<Type> {
        self.get_or_create_str_fixed_struct(capacity, span)
    }
    fn type_syntax_record_builtin_call(&mut self) {
        self.record_declaration_builtin_type_call_head(
            super::BuiltinTypeCallHead::FixedCapacityString,
        );
    }

    fn type_syntax_constructor(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let file = module.map(|module| self.module_registry.get_def(module).file_id);
        let key = match file {
            None => {
                let root = authority.file();
                let mut key = self.resolve_function_name_local(name, root);
                if key.is_none() {
                    let observer = self.declaration_type_observer.clone();
                    D::collect_free_function_signature(self, name, Some(root))?;
                    self.declaration_type_observer = observer;
                    key = self.resolve_function_name_local(name, root);
                }
                key
            }
            Some(file) => {
                let observer = self.declaration_type_observer.clone();
                D::collect_free_function_signature(self, name, Some(file))?;
                self.declaration_type_observer = observer;
                self.resolve_function_name_local(name, file)
            }
        };
        let Some(key) = key else { return Ok(None) };
        let Some(info) = self
            .functions
            .get(&key)
            .copied()
            .filter(|info| file.is_none_or(|file| info.file_id == file))
        else {
            return Ok(None);
        };
        self.record_declaration_type_call_head(key, info);
        let names = self.param_arena.names(info.params);
        let comptime = self.param_arena.comptime(info.params);
        let type_parameters = self.comptime_type_param_flags(&info);
        let parameters = names
            .iter()
            .zip(comptime)
            .zip(type_parameters)
            .map(
                |((&name, &is_comptime), is_type)| crate::SemanticTypeConstructorParameter {
                    name,
                    is_comptime,
                    is_type,
                },
            )
            .collect::<Vec<_>>();
        Ok(Some(crate::SemanticTypeConstructorHead {
            key,
            site: info.file_id,
            parameters: parameters.into(),
            returns_type: self.function_returns_type(&info),
            is_public: info.is_pub,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                self.get_file_path(info.file_id),
            ),
            defining_file: std::sync::Arc::from(
                self.get_file_path(info.file_id).unwrap_or("<unknown>"),
            ),
        }))
    }

    fn type_syntax_reduce_constructor(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        let type_arguments = type_arguments.iter().copied().collect::<HashMap<_, _>>();
        let value_arguments = value_arguments.iter().copied().collect::<HashMap<_, _>>();
        self.reduce_type_ctor_body(head.key, &type_arguments, &value_arguments, span)
            .map_err(|error| Self::label_ctor_instantiation_site(error, span))
    }

    fn type_syntax_value_const(&self, file: FileId, name: Spur) -> Option<super::info::ConstInfo> {
        self.resolve_const_info_in_file(name, file).cloned()
    }
    fn type_syntax_recover_const(
        &mut self,
        file: FileId,
        name: Spur,
    ) -> CompileResult<Option<ConstValue>> {
        D::resolve_indexed_const(self, name, file)
    }
    fn type_syntax_record_named_const_dependency(&mut self, file: FileId, name: String) {
        self.record_named_const_dependency(super::NamedConstDependencyTargetEvent::ValueConst {
            file: file.index(),
            name,
        });
    }
    fn type_syntax_out_of_scope_const_hint(&self, symbol: Spur, exclude: FileId) -> String {
        let mut modules = self
            .value_consts()
            .filter(|((file, name), info)| {
                *file != exclude
                    && *name == symbol
                    && info.is_pub
                    && info.value.as_int_value().is_some()
            })
            .filter_map(|((file, _), _)| self.file_paths.get(file).map(String::as_str))
            .collect::<Vec<_>>();
        modules.sort_unstable();
        modules.dedup();
        match modules.as_slice() {
            [] => String::new(),
            _ => format!(
                "; an integer constant of that name is declared in {} — import that module and bind a file-level `const` (for example `const {}: i32 = <module>.{};`) to use it as an array length here",
                modules.join(", "),
                self.interner.resolve(&symbol),
                self.interner.resolve(&symbol)
            ),
        }
    }
    fn type_syntax_dependencies(
        &self,
        ty: Type,
    ) -> Vec<(FileId, String, super::DeclarationTypeDependencyTargetKind)> {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self
                    .type_pool
                    .struct_metadata(id)
                    .expect("resolved declaration type names a registered struct identity");
                // Membership, not the generated-name prefix (RUE-1050).
                (!def.is_builtin && !self.anonymous_struct_ids.contains(&id))
                    .then_some((
                        def.file_id,
                        def.name.to_string(),
                        super::DeclarationTypeDependencyTargetKind::Struct,
                    ))
                    .into_iter()
                    .collect()
            }
            TypeKind::Enum(id) => {
                let def = self
                    .type_pool
                    .enum_metadata(id)
                    .expect("resolved declaration type names a registered enum identity");
                vec![(
                    def.file_id,
                    def.name.to_string(),
                    super::DeclarationTypeDependencyTargetKind::Enum,
                )]
            }
            TypeKind::Array(id) => self.type_syntax_dependencies(self.type_pool.array_def(id).0),
            TypeKind::PtrConst(id) => {
                self.type_syntax_dependencies(self.type_pool.ptr_const_def(id))
            }
            TypeKind::PtrMut(id) => self.type_syntax_dependencies(self.type_pool.ptr_mut_def(id)),
            _ => Vec::new(),
        }
    }
    fn type_syntax_flush_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        self.record_resolved_declaration_type_target(file, name, kind);
    }
}

impl<'source, D: DeclarationPhase> DeferredTypeSyntaxHost for Sema<'source, D> {
    fn type_syntax_comptime_value_failure(
        &self,
        failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
        span: Span,
    ) -> CompileError {
        use crate::SemanticResolutionError as E;
        use crate::SemanticTypeSyntaxFailure as F;

        match failure {
            E::Semantic(F::InvalidConstructorArity {
                constructor,
                expected,
                found,
                ..
            }) => CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "compile-time function '{}' expects {} comptime argument(s), but {} were provided",
                        constructor, expected, found
                    ),
                },
                span,
            ),
            E::Semantic(F::RuntimeConstructorParameter { constructor, .. }) => CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "call '{}' is not a compile-time value; all of its parameters must be comptime",
                        constructor
                    ),
                },
                span,
            ),
            failure => self.type_syntax_compile_error(failure, span),
        }
    }

    fn type_syntax_function_info(&self, key: Spur) -> FunctionInfo {
        *self
            .functions
            .get(&key)
            .expect("selected constructor has function metadata")
    }

    fn type_syntax_parameter_types(&self, function: FunctionInfo) -> Vec<Type> {
        self.param_arena.types(function.params).to_vec()
    }

    fn type_syntax_parameter_type_syntax(
        &self,
        function: FunctionInfo,
        index: usize,
    ) -> rue_rir::RirTypeSyntaxRef {
        let syntax = self
            .rir
            .params(function.rir_params(self.rir))
            .iter()
            .nth(index)
            .expect("selected constructor parameter is present")
            .ty;
        syntax
    }

    fn type_syntax_named_symbol(&self, syntax: rue_rir::RirTypeSyntaxRef) -> Option<Spur> {
        let rue_rir::RirTypeSyntaxNode::Named(symbol) = self.rir.type_syntax().node(syntax)? else {
            return None;
        };
        self.rir.type_syntax().symbol(*symbol).copied()
    }

    fn type_syntax_signature_substitutions_are_ready(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_params: &[Spur],
        value_params: &[Spur],
        type_substitutions: &HashMap<Spur, Type>,
        value_substitutions: &HashMap<Spur, ConstValue>,
    ) -> bool {
        self.deferred_rir_signature_substitutions_are_ready(
            syntax,
            type_params,
            value_params,
            type_substitutions,
            value_substitutions,
        )
    }

    fn type_syntax_resolve_substituted_parameter_type(
        &mut self,
        function: &FunctionInfo,
        index: usize,
        declared_type: Type,
        type_substitutions: &HashMap<Spur, Type>,
        value_substitutions: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type> {
        self.resolve_substituted_param_type(
            function,
            index,
            declared_type,
            type_substitutions,
            value_substitutions,
        )
    }

    fn type_syntax_type_mentions_type_param(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        parameters: &DeferredParameterFacts<'_>,
    ) -> bool {
        self.rir_type_mentions_parameter(syntax, |symbol| parameters.contains_type(symbol))
    }

    fn type_syntax_type_mentions_value_param(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        parameters: &DeferredParameterFacts<'_>,
    ) -> bool {
        self.rir_type_mentions_parameter(syntax, |symbol| parameters.contains_value(symbol))
    }

    fn type_syntax_resolve_structured_type(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
    ) -> CompileResult<Type> {
        self.resolve_rir_type(syntax, span)
    }

    fn type_syntax_type_display_name(&self, ty: Type) -> String {
        ty.safe_name_with_pool(Some(&self.type_pool))
    }

    fn type_syntax_validate_deferred_value(
        &self,
        concrete: Option<ConstValue>,
        found: Option<Type>,
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        value_name: &str,
        span: Span,
    ) -> CompileResult<()> {
        self.validate_deferred_value_result(
            concrete,
            found,
            expected,
            contract,
            require_integer,
            value_name,
            span,
        )
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// Get a human-readable name for a type.
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
        // A constructor-produced anonymous type prints its instantiation
        // spelling (`ArrayBuf(i64)`), not its internal structural name
        // (RUE-610; recorded by `reduce_type_ctor_body`).
        if let Some(display) = self.ctor_type_displays.get(&ty) {
            return display.clone();
        }
        match ty.kind() {
            TypeKind::I8 => "i8".to_string(),
            TypeKind::I16 => "i16".to_string(),
            TypeKind::I32 => "i32".to_string(),
            TypeKind::I64 => "i64".to_string(),
            TypeKind::U8 => "u8".to_string(),
            TypeKind::U16 => "u16".to_string(),
            TypeKind::U32 => "u32".to_string(),
            TypeKind::U64 => "u64".to_string(),
            TypeKind::Bool => "bool".to_string(),
            TypeKind::Unit => "()".to_string(),
            TypeKind::Never => "!".to_string(),
            TypeKind::Error => "<error>".to_string(),
            // Nominal strings and generated string views are ordinary structs.
            TypeKind::Struct(struct_id) => self
                .type_pool
                .struct_metadata(struct_id)
                .expect("struct type must have declaration metadata")
                .name
                .to_string(),
            TypeKind::Enum(enum_id) => self
                .type_pool
                .enum_metadata(enum_id)
                .expect("enum type must have declaration metadata")
                .name
                .to_string(),
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                format!("[{}; {}]", self.format_type_name(element_type), length)
            }
            TypeKind::PtrConst(ptr_id) => {
                let pointee = self.type_pool.ptr_const_def(ptr_id);
                format!("ptr const {}", self.format_type_name(pointee))
            }
            TypeKind::PtrMut(ptr_id) => {
                let pointee = self.type_pool.ptr_mut_def(ptr_id);
                format!("ptr mut {}", self.format_type_name(pointee))
            }
            TypeKind::Module(_) => "<module>".to_string(),
            TypeKind::ComptimeType => "type".to_string(),
        }
    }

    /// Point a container well-formedness rejection (E0499, RUE-388/RUE-646) at
    /// the user's type-constructor application. The gate raises inside the
    /// constructor's own body (`@require_droppable(T)` in std source), so
    /// without this label the user's file never appears in the diagnostic
    /// (RUE-610). Other reduction errors already carry their own spans.
    pub(crate) fn label_ctor_instantiation_site(err: CompileError, span: Span) -> CompileError {
        if matches!(err.kind, ErrorKind::ContainerElementIsLinear { .. }) {
            err.with_label("required by the type-constructor application here", span)
        } else {
            err
        }
    }

    /// Check if a type is a Copy type.
    /// This differs from Type::is_copy() because it can look up struct definitions
    /// to check if a struct is marked with @copy.
    pub(crate) fn is_type_copy(&self, ty: Type) -> bool {
        match ty.kind() {
            // Primitive Copy types
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit => true,
            // An enum is Copy iff every variant payload is Copy (RUE-221:
            // enum multiplicity is the join of its variants' payload
            // multiplicities, lattice Copy ⊑ Affine ⊑ Linear). A
            // discriminant-only (C-like) enum has no payloads and is Copy.
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                enum_def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .all(|&ty| self.is_type_copy(ty))
            }
            // Never and Error are Copy for convenience
            TypeKind::Never | TypeKind::Error => true,
            // Struct types: check if marked with @copy
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_copy
            }
            // Note: String is now handled via TypeKind::Struct with is_builtin
            // Arrays are Copy if their element type is Copy
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.is_type_copy(element_type)
            }
            // Module types are Copy (they're just compile-time namespace references)
            TypeKind::Module(_) => true,
            // ComptimeType is Copy (only exists at comptime anyway)
            TypeKind::ComptimeType => true,
            // Pointer types are Copy (they're just addresses)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => true,
        }
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    ///
    /// This handles the conversion of Type::new_array(id) to InferType::Array
    /// by looking up the array definition to get element type and length.
    pub(crate) fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                let element_infer = self.type_to_infer_type(element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length,
                }
            }
            // All other types wrap directly
            _ => InferType::Concrete(ty),
        }
    }
    /// Get (or lazily create) the synthetic 2-field struct that represents the
    /// second-class slice type `[T]` (ADR-0043, RUE-322).
    ///
    /// A slice is a fat pointer `{ ptr: ptr const T, len: u64 }`. Rather than
    /// invent a new `TypeKind`, the slice is modeled as a `@copy` synthetic
    /// struct so it flows through the existing multi-slot aggregate ABI, field
    /// reads (for `.len()`), and by-ref (`borrow`) parameter passing — exactly
    /// like the builtin `String` fat pointer. The struct is keyed by its slice
    /// syntax name (`[i32]`), so repeated references share one `StructId`.
    ///
    /// The shared syntax driver resolves the element before invoking this
    /// materializer. The pointer field is `ptr const T` so that `s[i]` can lower to
    /// `@ptr_read(@ptr_offset(ptr, i))` with the correct element stride.
    pub(crate) fn get_or_create_slice_struct_from_element(
        &mut self,
        type_name: &str,
        element_ty: Type,
        _span: Span,
    ) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let type_sym = self.interner.get_or_intern(type_name);
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        let ptr_type_id = self.type_pool.intern_ptr_const_from_type(element_ty);
        let ptr_ty = Type::new_ptr_const(ptr_type_id);

        let struct_def = StructDef {
            name: Arc::from(type_name),
            fields: vec![
                StructField {
                    name: "ptr".to_string(),
                    ty: ptr_ty,
                },
                StructField {
                    name: "len".to_string(),
                    ty: Type::U64,
                },
            ],
            // A slice is a copyable view (no ownership of the backing store).
            is_copy: true,
            is_linear: false,
            destructor: None,
            // Marked builtin so it never participates in user drop-glue etc.
            is_builtin: true,
            is_pub: true,
            file_id: rue_span::FileId::new(0),
        };
        let (struct_id, _) = self.type_pool.register_struct(type_sym, struct_def);
        self.generated_structs.insert(type_sym, struct_id);
        Ok(Type::new_struct(struct_id))
    }

    /// Get (or lazily create) the synthetic 2-field struct that represents the
    /// `str` string type (ADR-0043 Phase 3, RUE-324).
    ///
    /// `str` is `[u8]` + the UTF-8 byte-string convention (ADR-0035): a
    /// read-only slice of bytes `{ ptr: ptr const u8, len: u64 }`, sharing the
    /// slice fat-pointer representation so `.len()` and byte-indexing `s[i]`
    /// reuse the slice machinery verbatim. Unlike a plain `[T]` slice, `str` is
    /// **first-class**: string literals are static-backed (their bytes live in
    /// `.rodata`, which cannot dangle), so a `str` value is storable, `Copy`,
    /// and reassignable — it is exempt from the second-class-escape rule. The
    /// struct is keyed by the name `str`, so every reference shares one
    /// `StructId`.
    pub(crate) fn get_or_create_str_struct(&mut self, span: Span) -> CompileResult<Type> {
        super::ordinary_engine::OrdinaryBodyEngine::new(self).get_or_create_str_struct(span)
    }

    /// Requalify destructor symbols for struct names that span files
    /// (RUE-571). Registration (`register_destructor`) runs per-file before
    /// ambiguity is knowable, so it records the bare `Type.__drop`; once
    /// declarations are complete this re-points colliding entries at their
    /// file-qualified form, so every consumer of `StructDef::destructor`
    /// (drop glue in CFG build and both codegen backends) agrees with the
    /// definition side, which builds its name via [`Self::destructor_symbol`].
    pub(crate) fn requalify_colliding_destructor_symbols(&mut self) {
        let ids: Vec<StructId> = self
            .structs_by_file_name
            .values()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for struct_id in ids {
            if self.type_pool.struct_def(struct_id).destructor.is_none() {
                continue;
            }
            let qualified = self.destructor_symbol(struct_id);
            let def = self.type_pool.struct_def(struct_id);
            if def.destructor.as_deref() != Some(qualified.as_str()) {
                self.type_pool
                    .requalify_struct_destructor(struct_id, qualified);
            }
        }
    }

    /// The type-name component of function symbols for `struct_id` (RUE-571):
    /// delegates to [`TypeInternPool::struct_symbol_name`], the single
    /// definition shared with the drop-glue generator and both codegen
    /// backends.
    pub(crate) fn symbol_type_name(&self, struct_id: StructId) -> String {
        self.type_pool.struct_symbol_name(struct_id)
    }

    /// Symbol for a method (`Type.method`) or associated function
    /// (`Type::method`) of `struct_id`. Named user types are unconditionally
    /// file-qualified (ADR-0066, RUE-1089), while builtins remain bare.
    /// Definition sites (function-body analysis) and call sites (method /
    /// assoc-fn call analysis) must both build the name through this helper so
    /// they meet in AIR/codegen.
    pub(crate) fn method_symbol(
        &self,
        struct_id: StructId,
        method: &str,
        has_self: bool,
    ) -> String {
        let type_name = self.symbol_type_name(struct_id);
        if has_self {
            format!("{}.{}", type_name, method)
        } else {
            format!("{}::{}", type_name, method)
        }
    }

    /// Symbol for the destructor of `struct_id` (`Type.__drop`). Named user
    /// types are unconditionally file-qualified (ADR-0066, RUE-1089), while
    /// builtins remain bare.
    pub(crate) fn destructor_symbol(&self, struct_id: StructId) -> String {
        format!("{}.__drop", self.symbol_type_name(struct_id))
    }

    /// Get (or lazily create) the synthetic 2-field struct representing a
    /// fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326).
    ///
    /// `Str(N)` is the **fixed string rung** — `[u8; N]` + the UTF-8
    /// byte-string convention (ADR-0035) — the string analogue of the fixed
    /// array `[T; N]`: no heap, no allocator, a value type holding up to `N`
    /// bytes plus a current byte length. Each distinct capacity is its own
    /// named struct (`Str(8)`, keyed by that canonical name) so every reference
    /// to the same capacity shares one `StructId`.
    ///
    /// Representation note (this phase): `Str(N)` reuses the `str` fat-pointer
    /// shape `{ ptr: ptr const u8, len: u64 }`, and construction from a string
    /// literal is currently static-backed (bytes in `.rodata`, which cannot
    /// dangle). This makes `.len()`, byte-indexing, and coercion to `str` reuse
    /// the `str` machinery verbatim while the capacity `N` enforces the
    /// literal-fits legality rule. Genuine inline-stack storage and mutation
    /// (`push`/append) are deferred; for the literal-only, immutable surface of
    /// this phase the two are observationally identical.
    pub(crate) fn get_or_create_str_fixed_struct(
        &mut self,
        capacity: u64,
        span: Span,
    ) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let name = format!("Str({})", capacity);
        let type_sym = self.interner.get_or_intern(&name);
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        let ptr_type_id = self.type_pool.intern_ptr_const_from_type(Type::U8);
        let ptr_ty = Type::new_ptr_const(ptr_type_id);

        let struct_def = StructDef {
            name: Arc::from(name.as_str()),
            fields: vec![
                StructField {
                    name: "ptr".to_string(),
                    ty: ptr_ty,
                },
                StructField {
                    name: "len".to_string(),
                    ty: Type::U64,
                },
            ],
            is_copy: true,
            is_linear: false,
            destructor: None,
            is_builtin: true,
            is_pub: true,
            file_id: rue_span::FileId::new(0),
        };
        let (struct_id, _) = self.type_pool.register_struct(type_sym, struct_def);
        self.generated_structs.insert(type_sym, struct_id);
        let _ = span;
        Ok(Type::new_struct(struct_id))
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Is `ty` the synthetic `str` struct (ADR-0043 Phase 3, RUE-324)? Detected
    /// by the struct name being exactly `str`. Used to route string literals and
    /// slice-style `.len()`/index operations through the fat-pointer paths while
    /// keeping `str` first-class (exempt from the slice second-class rule).
    pub(crate) fn is_str_struct(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            &*self.body_type_pool().struct_def(struct_id).name == "str"
        } else {
            false
        }
    }

    /// Is `ty` a fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326)?
    /// Detected by the struct name matching `Str(<digits>)`, mirroring the
    /// name-keyed detection used for `str` and slices.
    pub(crate) fn is_str_fixed_struct(&self, ty: Type) -> bool {
        self.str_fixed_capacity(ty).is_some()
    }

    /// If `ty` is a fixed-capacity string `Str(N)`, return its capacity `N`
    /// (ADR-0043 Phase 5, RUE-326); otherwise `None`. The capacity is parsed
    /// back out of the canonical struct name `Str(<N>)`.
    pub(crate) fn str_fixed_capacity(&self, ty: Type) -> Option<u64> {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let name = &self.body_type_pool().struct_def(struct_id).name;
            crate::types::fixed_string_capacity(name)
        } else {
            None
        }
    }

    /// Is `ty` `str`-like — either the `str` slice view or a fixed-capacity
    /// `Str(N)` (ADR-0043 Phases 3/5)? Both share the 2-word `{ptr, len}`
    /// representation and the UTF-8 byte-string convention, so string-literal
    /// materialization, `.len()`, packed byte-indexing, and by-value passing all
    /// treat them identically. The capacity-fits legality rule is the only place
    /// `Str(N)` diverges from `str`.
    pub(crate) fn is_str_like(&self, ty: Type) -> bool {
        self.is_str_struct(ty) || self.is_str_fixed_struct(ty)
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// If `ty` is a synthetic slice struct `[T]` (ADR-0043, RUE-322), return its
    /// element type `T`; otherwise `None`. Fixed arrays have their own
    /// [`TypeKind::Array`] identity, so any struct with the synthetic bracketed
    /// name is a slice.
    pub(crate) fn slice_element_type(&self, ty: Type) -> Option<Type> {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let def = self.type_pool.struct_def(struct_id);
            // A `[T]` slice, or the `str` string type which is `[u8]` + UTF-8
            // (ADR-0043 Phase 3, RUE-324): both are `{ptr: ptr const E, len}`
            // fat pointers, so `.len()` and `s[i]` reuse one path.
            if crate::types::is_slice_struct_name(&def.name)
                || crate::types::is_string_view_struct_name(&def.name)
            {
                // Field 0 is `ptr const T`; recover T from its pointee.
                if let TypeKind::PtrConst(ptr_id) = def.fields[0].ty.kind() {
                    return Some(self.type_pool.ptr_const_def(ptr_id));
                }
            }
        }
        None
    }

    /// Reject a slice type appearing outside argument position. When `type_sym`
    /// is slice syntax and `--preview slices` is enabled, this returns the
    /// second-class-escape error `kind` (E0487/E0488/E0489); otherwise it
    /// returns `Ok(())` and the caller proceeds. The preview gate is checked
    /// first so that, without the flag, the user still sees the uniform
    /// "requires preview feature" message rather than a bespoke slice error.
    pub(crate) fn reject_slice_escape(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
        kind: ErrorKind,
    ) -> CompileResult<()> {
        if matches!(
            self.rir.type_syntax().node(syntax),
            Some(rue_rir::RirTypeSyntaxNode::Slice { .. })
        ) {
            self.require_preview(PreviewFeature::Slices, "the slice type `[T]`", span)?;
            return Err(CompileError::new(kind, span));
        }
        Ok(())
    }

    pub(crate) fn resolve_rir_type(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
    ) -> CompileResult<Type> {
        self.resolve_rir_type_with_substitutions(syntax, span, None, None)
    }

    pub(crate) fn resolve_rir_type_with_substitutions(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
        type_substitutions: Option<&HashMap<Spur, Type>>,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<Type> {
        let arena = self.rir.type_syntax().clone();
        let interner = self.interner;
        let mut provider = TypeSyntaxProvider::new(
            self,
            span,
            TypeRootAuthority::in_file(span.file_id),
            SemaTypeResolutionContext::Type,
            type_substitutions,
            value_substitutions,
        );
        let result = crate::resolve_structured_semantic_type_syntax_with(
            &mut provider,
            &span.file_id,
            &arena,
            syntax,
            |symbol| interner.resolve(symbol),
        );
        provider.flush_observed_type_dependencies();
        result.map_err(|failure| semantic_type_syntax_compile_error(interner, failure, span))
    }

    pub(crate) fn resolve_structured_type_syntax_with_epoch_facts(
        &mut self,
        arena: &rue_rir::RirTypeSyntaxArena<Spur>,
        syntax: rue_rir::RirTypeSyntaxRef,
        root_file: FileId,
        span: Span,
        type_substitutions: Option<&HashMap<Spur, Type>>,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> Result<Type, crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>> {
        let interner = self.interner;
        let mut provider = TypeSyntaxProvider::new(
            self,
            span,
            TypeRootAuthority::in_file(root_file),
            SemaTypeResolutionContext::Type,
            type_substitutions,
            value_substitutions,
        );
        let result = crate::resolve_structured_semantic_type_syntax_with(
            &mut provider,
            &root_file,
            arena,
            syntax,
            |symbol| interner.resolve(symbol),
        );
        provider.flush_observed_type_dependencies();
        result
    }

    #[cfg(test)]
    pub(crate) fn resolve_structured_type_syntax_for_testing(
        &mut self,
        arena: &rue_rir::RirTypeSyntaxArena<Spur>,
        syntax: rue_rir::RirTypeSyntaxRef,
        span: Span,
    ) -> CompileResult<Type> {
        self.resolve_structured_type_syntax_with_epoch_facts(
            arena,
            syntax,
            span.file_id,
            span,
            None,
            None,
        )
        .map_err(|failure| self.type_syntax_compile_error(failure, span))
    }

    #[cfg(test)]
    pub(crate) fn validate_deferred_structured_type_for_testing(
        &mut self,
        arena: &rue_rir::RirTypeSyntaxArena<Spur>,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_params: &[Spur],
        span: Span,
    ) -> CompileResult<Option<Type>> {
        fn contains_value_call(
            arena: &rue_rir::RirTypeSyntaxArena<Spur>,
            syntax: rue_rir::RirTypeSyntaxRef,
        ) -> bool {
            if matches!(
                arena.node(syntax),
                Some(rue_rir::RirTypeSyntaxNode::ValueCall { .. })
            ) {
                return true;
            }
            let mut found = false;
            arena.visit_child_references(syntax, |child| {
                found |= contains_value_call(arena, child);
            }) && found
        }

        let parameters = DeferredParameterFacts::new(type_params, &[], &[]);
        let interner = self.interner;
        let mut provider = DeferredTypeSyntaxProvider::new(self, span, &parameters);
        let result = crate::resolve_structured_semantic_type_syntax_with(
            &mut provider,
            &span.file_id,
            arena,
            syntax,
            |symbol| interner.resolve(symbol),
        );
        provider.flush_observed_type_dependencies();
        match result {
            Ok(DeferredTypeResolution::Resolved(ty)) => Ok(Some(ty)),
            Ok(DeferredTypeResolution::Pending) => Ok(None),
            Err(failure) if contains_value_call(arena, syntax) => Err(
                DeferredTypeSyntaxHost::type_syntax_comptime_value_failure(self, failure, span),
            ),
            Err(failure) => Err(self.type_syntax_compile_error(failure, span)),
        }
    }

    fn type_syntax_compile_error(
        &self,
        failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
        span: Span,
    ) -> CompileError {
        semantic_type_syntax_compile_error(self.interner, failure, span)
    }

    pub(crate) fn record_resolved_declaration_type(&mut self, ty: Type) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self
                    .type_pool
                    .struct_metadata(id)
                    .expect("resolved declaration type names a registered struct identity");
                // Membership, not the generated-name prefix (RUE-1050).
                if !def.is_builtin && !self.anonymous_struct_ids.contains(&id) {
                    self.record_resolved_declaration_type_target(
                        def.file_id,
                        def.name.to_string(),
                        super::DeclarationTypeDependencyTargetKind::Struct,
                    );
                }
            }
            TypeKind::Enum(id) => {
                let def = self
                    .type_pool
                    .enum_metadata(id)
                    .expect("resolved declaration type names a registered enum identity");
                self.record_resolved_declaration_type_target(
                    def.file_id,
                    def.name.to_string(),
                    super::DeclarationTypeDependencyTargetKind::Enum,
                );
            }
            TypeKind::Array(id) => {
                self.record_resolved_declaration_type(self.type_pool.array_def(id).0)
            }
            TypeKind::PtrConst(id) => {
                self.record_resolved_declaration_type(self.type_pool.ptr_const_def(id));
            }
            TypeKind::PtrMut(id) => {
                self.record_resolved_declaration_type(self.type_pool.ptr_mut_def(id));
            }
            _ => {}
        }
    }

    fn record_resolved_declaration_type_target(
        &mut self,
        target_file: FileId,
        target_name: String,
        target_kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        let event = super::DeclarationTypeDependencyEvent {
            source_token: self
                .body_dependency_observer
                .as_ref()
                .and_then(super::AnalyzedBodyOwnerEvent::token),
            source_file: source_file.index(),
            source_name,
            source_owner_name,
            source_kind,
            dependency_kind,
            target_file: target_file.index(),
            target_name,
            target_kind,
        };
        if self.declaration_type_dependency_index.insert(event.clone()) {
            self.declaration_type_dependencies.push(event);
            self.body_analysis_work.declaration_type_dependency_events += 1;
        }
    }

    /// Resolve a type-annotation symbol to a `Type`, consulting the analysis
    /// context's local comptime type bindings at every level of a composite
    /// annotation (RUE-263).
    ///
    /// [`resolve_type`] takes no context, so it can validate a *scalar*
    /// comptime-type-var annotation (`let p: P`, which the caller special-cases
    /// before ever reaching resolution) but not a *composite* one: `[P; 2]` or
    /// `ptr const P` misses in the local-binding map as a whole symbol, and the
    /// context-free recursion then can't resolve the inner `P` (it isn't a named
    /// struct/enum), yielding a spurious E0204. This variant threads
    /// `ctx.comptime_type_vars` through the recursion so an inner
    /// element/pointee that is a local comptime type var resolves. Non-composite
    /// leaves and genuinely-unknown names fall through to [`resolve_type`],
    /// keeping the primitive/struct/enum resolution, privacy checks (E0460), and
    /// the E0204 for a truly unknown type in one place so the two paths can't
    /// drift.
    ///
    /// Array lengths are resolved through `ctx.comptime_value_vars` (RUE-271):
    /// a length naming an enclosing `comptime N: i32` value parameter (`[i32; N]`
    /// / `[P; N]`) binds to its concrete value at each specialization, then
    /// falls back to file-level `const`s and literals like [`resolve_type`]. A
    /// length that resolves to none of these (a runtime parameter) still gets a
    /// clean E0481. The specialized array type is interned into the same pool
    /// the CFG builder later sees (the pool is transferred *after* specialization,
    /// RUE-282), so drop analysis of a comptime-value-length local array no
    /// longer hits an out-of-bounds `ArrayTypeId`.
    ///
    /// [`resolve_type`]: Sema::resolve_type
    /// Like [`Self::resolve_qualified_type_name`], but with the file whose
    /// imports anchor the path's first segment made explicit. The comptime
    /// engine resolves a member-access type path (`std.strbuf.StrBuf` as a
    /// value-position type-constructor argument) against its environment's
    /// `defining_file`, the same file the qualified type-annotation path walks
    /// (RUE-948, mirroring the RUE-511/RUE-609 receiver walk).
    /// Like [`Self::resolve_type_module_prefix`], but with the file whose
    /// imports anchor the walk's first segment made explicit. The comptime
    /// engine resolves against its environment's `defining_file` (RUE-511),
    /// which is authoritative even when the expression's span is a default
    /// span with no file context (RUE-609).
    pub(crate) fn resolve_type_module_prefix_with_epoch_facts(
        &mut self,
        root_file: FileId,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<(crate::types::ModuleId, Option<FileId>, String)> {
        let mut provider = TypeSyntaxProvider::new(
            self,
            span,
            TypeRootAuthority::in_file(root_file),
            SemaTypeResolutionContext::Type,
            None,
            None,
        );
        let resolved = crate::resolve_semantic_module_path(&mut provider, &root_file, segments)
            .map_err(|failure| module_path_resolution_compile_error(failure, span))?;
        provider.flush_observed_type_dependencies();

        let module_id = resolved.module;
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = Some(module_def.file_id);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or_else(|| module_def.file_path.clone());
        Ok((module_id, module_file_id, module_file_path))
    }

    /// Look up a module binding in the declaration namespace.
    ///
    /// While declaration payloads are being bound, a qualified type may name
    /// an import whose constant appears later in source order. Resolve that
    /// dependency through the declaration index, exactly like any other
    /// constant dependency. Once declaration binding completes, the table is
    /// authoritative: body analysis never rediscovers imports or mutates the
    /// source-declaration namespace after the [`super::BoundSema`] boundary.
    pub(crate) fn resolve_module_binding_in_file(
        &mut self,
        file_id: FileId,
        name: Spur,
    ) -> CompileResult<Option<super::info::ConstInfo>> {
        if let Some(binding) = self.module_binding(&(file_id, name)) {
            return Ok(Some(binding.clone()));
        }
        D::resolve_indexed_const(self, name, file_id)?;
        Ok(self.module_binding(&(file_id, name)).cloned())
    }

    /// Return one flag per source parameter identifying declarations of the
    /// form `comptime T: type`.
    ///
    /// This kind cannot be reconstructed from the semantic parameter type:
    /// both a type parameter and a deferred comptime value parameter such as
    /// `comptime value: T` carry [`Type::COMPTIME_TYPE`] until substitution.
    /// The original RIR declaration is therefore the source of truth for
    /// specialization-stream routing and runtime erasure.
    pub(crate) fn comptime_type_param_flags(&self, function: &FunctionInfo) -> Vec<bool> {
        let flags: Vec<bool> = self
            .rir
            .params(function.rir_params(self.rir))
            .iter()
            .map(|param| param.is_comptime && self.rir_type_is_named(param.ty, "type"))
            .collect();
        debug_assert_eq!(flags.len(), function.params.len());
        flags
    }

    /// Resolve one parameter of a generic function under a concrete
    /// specialization.
    ///
    /// Declaration gathering uses [`Type::COMPTIME_TYPE`] as a placeholder
    /// for runtime parameter types that mention comptime type/value
    /// parameters (`x: T`, `a: [T; N]`, and so on). Both the call site and the
    /// specialized callee must replace that placeholder through this exact
    /// path. Retaining it after the substitution is known would let them
    /// independently classify the parameter and silently disagree.
    pub(crate) fn resolve_substituted_param_type(
        &mut self,
        function: &FunctionInfo,
        param_index: usize,
        declared: Type,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type> {
        if declared != Type::COMPTIME_TYPE {
            return Ok(declared);
        }

        let rir_params = self.rir.params(function.rir_params(self.rir));
        let param = rir_params.get(param_index).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "generic parameter index {} is missing from its RIR declaration",
                    param_index
                )),
                function.span,
            )
        })?;
        self.resolve_rir_type_with_substitutions(
            param.ty,
            function.span,
            Some(type_subst),
            Some(value_subst),
        )
    }

    /// Resolve the return half of the same specialized signature contract.
    /// Caller and callee must agree on it just as strictly as on parameter
    /// widths; silently retaining the placeholder (or substituting unit) would
    /// merely move the disagreement to the returned value.
    pub(crate) fn resolve_substituted_return_type(
        &mut self,
        function: &FunctionInfo,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type> {
        if function.return_type != Type::COMPTIME_TYPE {
            return Ok(function.return_type);
        }
        self.resolve_rir_type_with_substitutions(
            function.return_type_syntax,
            function.span,
            Some(type_subst),
            Some(value_subst),
        )
    }

    fn record_declaration_builtin_type_call_head(&mut self, builtin: super::BuiltinTypeCallHead) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        self.declaration_builtin_type_call_head_dependencies.push(
            super::DeclarationBuiltinTypeCallHeadDependencyEvent {
                source_token: self
                    .body_dependency_observer
                    .as_ref()
                    .and_then(super::AnalyzedBodyOwnerEvent::token),
                source_file: source_file.index(),
                source_name,
                source_owner_name,
                source_kind,
                dependency_kind,
                builtin,
            },
        );
        self.body_analysis_work
            .declaration_type_call_head_dependency_events += 1;
    }

    pub(crate) fn resolve_array_length_with_epoch_facts(
        &mut self,
        length: &ArrayLen,
        span: Span,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        let mut provider = TypeSyntaxProvider::new(
            self,
            span,
            TypeRootAuthority::in_file(span.file_id),
            SemaTypeResolutionContext::Type,
            None,
            value_substitutions,
        );
        let result = provider.resolve_array_length_fact(span.file_id, length);
        provider.flush_observed_type_dependencies();
        result
    }
    /// Validate the non-deferred parts of a signature type that will otherwise
    /// be deferred until generic specialization.
    ///
    /// A composite signature such as `[T; 3]` cannot be resolved at declaration
    /// time because the element type is a comptime type parameter. Its length is
    /// still a declaration-time legality question, though: `[T; A]` must reject
    /// an undefined `A` immediately instead of surviving until specialization
    /// and becoming an ICE (RUE-381). Lengths may be literals, file constants, or
    /// comptime value parameters owned by the same function.
    ///
    /// Non-deferred leaf types are declaration-time legality questions too:
    /// `[i3; N]` mentions the comptime value parameter `N`, so the array type as
    /// a whole is deferred, but the element type `i3` is not deferred and should
    /// be reported as E0204 immediately instead of becoming a failed
    /// specialization substitution later.
    pub(crate) fn rir_type_mentions_parameter(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        matches: impl Copy + Fn(Spur) -> bool,
    ) -> bool {
        let arena = self.rir.type_syntax();
        if let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) = arena.node(syntax)
            && let Some(symbol) = arena.symbol(*symbol)
            && matches(*symbol)
        {
            return true;
        }
        let mut found = false;
        arena.visit_child_references(syntax, |child| {
            found |= self.rir_type_mentions_parameter(child, matches);
        }) && found
    }

    pub(crate) fn validate_deferred_rir_type(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        parameters: &DeferredParameterFacts<'_>,
        span: Span,
    ) -> CompileResult<()> {
        let arena = self.rir.type_syntax().clone();
        fn contains_value_call(
            arena: &rue_rir::RirTypeSyntaxArena<Spur>,
            syntax: rue_rir::RirTypeSyntaxRef,
        ) -> bool {
            if matches!(
                arena.node(syntax),
                Some(rue_rir::RirTypeSyntaxNode::ValueCall { .. })
            ) {
                return true;
            }
            let mut found = false;
            arena.visit_child_references(syntax, |child| {
                found |= contains_value_call(arena, child);
            }) && found
        }
        let value_call = contains_value_call(&arena, syntax);
        let interner = self.interner;
        let mut provider = DeferredTypeSyntaxProvider::new(self, span, parameters);
        let result = crate::resolve_structured_semantic_type_syntax_with(
            &mut provider,
            &span.file_id,
            &arena,
            syntax,
            |symbol| interner.resolve(symbol),
        );
        provider.flush_observed_type_dependencies();
        match result {
            Ok(DeferredTypeResolution::Resolved(_) | DeferredTypeResolution::Pending) => Ok(()),
            Err(failure) if value_call => Err(
                DeferredTypeSyntaxHost::type_syntax_comptime_value_failure(self, failure, span),
            ),
            Err(failure) => Err(semantic_type_syntax_compile_error(interner, failure, span)),
        }
    }

    fn record_declaration_type_call_head(&mut self, function_key: Spur, info: FunctionInfo) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        self.declaration_type_call_head_dependencies.push(
            super::DeclarationTypeCallHeadDependencyEvent {
                source_token: self
                    .body_dependency_observer
                    .as_ref()
                    .and_then(super::AnalyzedBodyOwnerEvent::token),
                source_file: source_file.index(),
                source_name,
                source_owner_name,
                source_kind,
                dependency_kind,
                callable_file: info.file_id.index(),
                callable_name: self
                    .interner
                    .resolve(&self.source_function_name(function_key))
                    .to_string(),
            },
        );
        self.body_analysis_work
            .declaration_type_call_head_dependency_events += 1;
    }

    /// Whether the declaration's source return annotation is literally `type`.
    ///
    /// `FunctionInfo::return_type` cannot answer this: declaration gathering
    /// also uses `COMPTIME_TYPE` as the placeholder for a dependent return such
    /// as `-> T`. Kind-sensitive call paths must therefore consult the original
    /// source symbol instead of the semantic placeholder.
    pub(crate) fn function_returns_type(&self, function: &FunctionInfo) -> bool {
        function.returns_type
    }

    pub(crate) fn rir_type_is_named(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        expected: &str,
    ) -> bool {
        let arena = self.rir.type_syntax();
        let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) = arena.node(syntax) else {
            return false;
        };
        arena
            .symbol(*symbol)
            .is_some_and(|symbol| self.interner.resolve(symbol) == expected)
    }

    /// Validate a source fragment used in type position. The optional result
    /// is the concrete type when the fragment is independent of the enclosing
    /// generic parameters; `None` means specialization must finish it.
    fn deferred_rir_signature_substitutions_are_ready(
        &self,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_params: &[Spur],
        value_params: &[Spur],
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> bool {
        let missing_type_params = type_params
            .iter()
            .copied()
            .filter(|name| !type_subst.contains_key(name))
            .collect::<Vec<_>>();
        let missing_value_params = value_params
            .iter()
            .copied()
            .filter(|name| !value_subst.contains_key(name))
            .collect::<Vec<_>>();
        let parameters =
            DeferredParameterFacts::new(&missing_type_params, &missing_value_params, &[]);
        !self.rir_type_mentions_parameter(syntax, |symbol| parameters.contains_type(symbol))
            && !self.rir_type_mentions_parameter(syntax, |symbol| parameters.contains_value(symbol))
    }

    fn validate_deferred_value_result(
        &self,
        value: Option<ConstValue>,
        found: Option<Type>,
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        expression: &str,
        span: Span,
    ) -> CompileResult<()> {
        if require_integer {
            if let Some(value) = value {
                let Some(integer) = value.as_int_value() else {
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "array length expression '{}' is not an integer",
                                expression
                            ),
                        },
                        span,
                    ));
                };
                if integer < 0 || u64::try_from(integer).is_err() {
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "array length expression '{}' is outside the valid range",
                                expression
                            ),
                        },
                        span,
                    ));
                }
            } else if let Some(found) = found
                && !found.is_integer()
            {
                return Err(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!(
                            "array length expression '{}' has non-integer type {}",
                            expression,
                            found.safe_name_with_pool(Some(&self.type_pool))
                        ),
                    },
                    span,
                ));
            }
        }

        let Some(expected) = expected else {
            return Ok(());
        };
        if let Some(value) = value {
            let (function_name, param_name) = contract.expect("value contracts name a parameter");
            return super::comptime_eval::validate_comptime_value_for_type_impl(
                self.interner,
                &self.type_pool,
                function_name,
                param_name,
                value,
                expected,
                span,
            );
        }
        if let Some(found) = found
            && found != expected
            && !(found.is_integer() && expected.is_integer())
        {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: expected.safe_name_with_pool(Some(&self.type_pool)),
                    found: found.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Get or create an array type for the given element type and length.
    pub(crate) fn get_or_create_array_type(
        &mut self,
        element_type: Type,
        length: u64,
    ) -> ArrayTypeId {
        self.type_pool.intern_array_from_type(element_type, length)
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Reject a type whose layout exceeds the implementation's maximum object
    /// size — [`MAX_TYPE_SLOTS`] ABI slots (Appendix C practical limit,
    /// RUE-561) — returning the slot count on success. Call this wherever a
    /// value of `ty` is MATERIALIZED — a local or temporary slot allocation, a
    /// by-value parameter, `@size_of` / `@align_of` — so the saturating
    /// fallback in [`Self::abi_slot_count`] is never observable.
    pub(crate) fn require_layout_slots(&self, ty: Type, span: Span) -> CompileResult<u32> {
        match self.checked_abi_slot_count(ty) {
            Some(slots) => Ok(slots as u32),
            None => Err(CompileError::new(
                ErrorKind::TypeTooLarge {
                    type_name: ty.safe_name_with_pool(Some(self.body_type_pool())),
                    max_slots: MAX_TYPE_SLOTS,
                },
                span,
            )),
        }
    }

    /// Reserve a cumulative local or parameter frame region without allowing
    /// individually valid layouts to overflow the function-wide displacement
    /// budget (RUE-780).
    pub(crate) fn reserve_frame_slots(
        &self,
        current: &mut u32,
        additional: u32,
        span: Span,
    ) -> CompileResult<u32> {
        let start = *current;
        *current =
            crate::layout::checked_function_frame_slots(start, additional).ok_or_else(|| {
                CompileError::new(
                    ErrorKind::FunctionFrameTooLarge {
                        max_bytes: crate::layout::MAX_FUNCTION_FRAME_BYTES,
                    },
                    span,
                )
            })?;
        Ok(start)
    }

    /// Checked companion to [`Self::abi_slot_count`]: `None` when the type's
    /// layout overflows or exceeds [`MAX_TYPE_SLOTS`] (RUE-561). Computed in
    /// u64 with checked arithmetic so large array lengths cannot truncate to
    /// zero slots or overflow the slot-count multiplication.
    pub(crate) fn checked_abi_slot_count(&self, ty: Type) -> Option<u64> {
        let slots = match ty.kind() {
            TypeKind::Array(array_type_id) => {
                let (element_type, length) = self.body_type_pool().array_def(array_type_id);
                let element_slots = self.checked_abi_slot_count(element_type)?;
                element_slots.checked_mul(length)?
            }
            TypeKind::Struct(struct_id) => {
                let struct_def = self.body_type_pool().struct_def(struct_id);
                let mut total = 0u64;
                for f in &struct_def.fields {
                    total = total.checked_add(self.checked_abi_slot_count(f.ty)?)?;
                }
                total
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self.body_type_pool().enum_def(enum_id);
                let mut max_payload = 0u64;
                for i in 0..enum_def.variant_count() {
                    let mut variant_slots = 0u64;
                    for &vty in enum_def.variant_payload(i) {
                        variant_slots =
                            variant_slots.checked_add(self.checked_abi_slot_count(vty)?)?;
                    }
                    max_payload = max_payload.max(variant_slots);
                }
                1 + max_payload
            }
            // Every other kind is 0 or 1 slots; delegate.
            _ => u64::from(self.abi_slot_count(ty)),
        };
        (slots <= MAX_TYPE_SLOTS).then_some(slots)
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    ///
    /// Layout arithmetic SATURATES (no overflow panic, no silent u32
    /// truncation — RUE-561); an oversized type is rejected with E0906 at
    /// every materialization site via [`Self::require_layout_slots`], so the
    /// saturated value is never used for real allocation.
    pub(crate) fn abi_slot_count(&self, ty: Type) -> u32 {
        self.body_type_pool().provisional_abi_slot_count(ty)
    }
}

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;

    use super::*;

    #[test]
    fn deferred_parameter_facts_index_only_the_large_case_and_keep_first_value_type() {
        let interner = ThreadedRodeo::new();
        let names = (0..9)
            .map(|index| interner.get_or_intern(format!("P{index}")))
            .collect::<Vec<_>>();
        let type_syntax = (0..9)
            .map(rue_rir::RirTypeSyntaxRef::from_u32)
            .collect::<Vec<_>>();
        let small_pairs = names[..8]
            .iter()
            .copied()
            .zip(type_syntax[..8].iter().copied())
            .collect::<Vec<_>>();
        let small = DeferredParameterFacts::new(&names[..8], &names[..8], &small_pairs);
        assert!(small.type_param_index.is_none());
        assert!(small.value_param_index.is_none());
        assert!(small.value_param_type_index.is_none());
        assert!(small.contains_type(names[7]));
        assert_eq!(small.value_type_syntax(names[7]), Some(type_syntax[7]));

        let mut large_pairs = names
            .iter()
            .copied()
            .zip(type_syntax.iter().copied())
            .collect::<Vec<_>>();
        large_pairs.insert(1, (names[0], type_syntax[8]));
        let large = DeferredParameterFacts::new(&names, &names, &large_pairs);
        assert!(large.type_param_index.is_some());
        assert!(large.value_param_index.is_some());
        assert!(large.value_param_type_index.is_some());
        assert!(large.contains_value(names[8]));
        assert_eq!(large.value_type_syntax(names[0]), Some(type_syntax[0]));
    }
}
