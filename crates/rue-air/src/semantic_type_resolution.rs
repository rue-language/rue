//! Canonical, value-only type-syntax resolution policy.
//!
//! Providers expose orthogonal declaration facts and materialization hooks.
//! This module alone owns syntax routing, namespace precedence, recursive
//! structural resolution, qualified-path walking, visibility, and constructor
//! argument binding.

use std::path::Path;
use std::sync::Arc;

use lasso::{Key, Spur};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticVisibilityDomain(Option<Arc<str>>);

impl SemanticVisibilityDomain {
    pub fn from_file_path(path: Option<&str>) -> Self {
        Self(path.and_then(|path| {
            Path::new(path)
                .parent()
                .map(|parent| Arc::<str>::from(parent.to_string_lossy().as_ref()))
        }))
    }

    pub fn is_visible_from(&self, accessing: &Self, is_public: bool) -> bool {
        is_public || accessing.0.is_none() || self.0.is_none() || accessing.0 == self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticModuleBinding<M, A> {
    pub target: M,
    pub site: A,
    pub is_public: bool,
    pub defining_domain: SemanticVisibilityDomain,
    pub defining_file: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTypeFact<T, A> {
    pub value: T,
    pub site: A,
    pub is_public: bool,
    pub defining_domain: SemanticVisibilityDomain,
    pub defining_file: Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticTypeFactKind {
    Struct,
    Enum,
    Constant,
    Function,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProviderError<E, F> {
    Abort(E),
    Failure(F),
}

impl SemanticTypeFactKind {
    pub fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Constant => "constant",
            Self::Function => "function",
        }
    }
}

pub trait SemanticModulePathProvider<S, M, A> {
    type Abort;
    type Failure;

    fn root_module_binding(
        &mut self,
        scope: &S,
        name: &str,
    ) -> Result<
        Option<SemanticModuleBinding<M, A>>,
        SemanticProviderError<Self::Abort, Self::Failure>,
    >;

    fn module_binding(
        &mut self,
        module: &M,
        name: &str,
    ) -> Result<
        Option<SemanticModuleBinding<M, A>>,
        SemanticProviderError<Self::Abort, Self::Failure>,
    >;

    fn module_display_name(&self, module: &M) -> Arc<str>;

    fn accessing_domain(&self, scope: &S) -> SemanticVisibilityDomain;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticResolvedModule<M, A> {
    pub module: M,
    pub site: A,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticModulePathFailure<A> {
    Empty,
    UnknownRoot {
        name: Arc<str>,
    },
    UnknownMember {
        module: Arc<str>,
        module_site: A,
        member: Arc<str>,
    },
    PrivateMember {
        member: Arc<str>,
        site: A,
        defining_file: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticResolutionError<E, P, F> {
    ProviderAbort(E),
    ProviderFailure(P),
    Semantic(F),
    ComptimeCallTypeArgument {
        constructor: Arc<str>,
        argument_index: usize,
        argument: Arc<str>,
        error: Box<SemanticResolutionError<E, P, F>>,
    },
}

impl<E, P, F> SemanticResolutionError<E, P, F> {
    fn map_semantic<G>(self, map: impl Fn(F) -> G + Copy) -> SemanticResolutionError<E, P, G> {
        match self {
            Self::ProviderAbort(error) => SemanticResolutionError::ProviderAbort(error),
            Self::ProviderFailure(error) => SemanticResolutionError::ProviderFailure(error),
            Self::Semantic(error) => SemanticResolutionError::Semantic(map(error)),
            Self::ComptimeCallTypeArgument {
                constructor,
                argument_index,
                argument,
                error,
            } => SemanticResolutionError::ComptimeCallTypeArgument {
                constructor,
                argument_index,
                argument,
                error: Box::new(error.map_semantic(map)),
            },
        }
    }
}

fn lift_provider<T, E, P, F>(
    result: Result<T, SemanticProviderError<E, P>>,
) -> Result<T, SemanticResolutionError<E, P, F>> {
    result.map_err(|error| match error {
        SemanticProviderError::Abort(error) => SemanticResolutionError::ProviderAbort(error),
        SemanticProviderError::Failure(error) => SemanticResolutionError::ProviderFailure(error),
    })
}

pub fn resolve_semantic_module_path<S, M, A, P>(
    provider: &mut P,
    root_scope: &S,
    segments: &[&str],
) -> Result<
    SemanticResolvedModule<M, A>,
    SemanticResolutionError<P::Abort, P::Failure, SemanticModulePathFailure<A>>,
>
where
    M: Clone,
    A: Clone,
    P: SemanticModulePathProvider<S, M, A>,
{
    use SemanticModulePathFailure as F;
    use SemanticResolutionError as E;

    let Some((first_name, rest)) = segments.split_first() else {
        return Err(E::Semantic(F::Empty));
    };
    let first =
        lift_provider(provider.root_module_binding(root_scope, first_name))?.ok_or_else(|| {
            E::Semantic(F::UnknownRoot {
                name: Arc::from(*first_name),
            })
        })?;
    let accessing = provider.accessing_domain(root_scope);
    if !first
        .defining_domain
        .is_visible_from(&accessing, first.is_public)
    {
        return Err(E::Semantic(F::PrivateMember {
            member: Arc::from(*first_name),
            site: first.site,
            defining_file: first.defining_file,
        }));
    }

    let mut resolved = SemanticResolvedModule {
        module: first.target,
        site: first.site,
    };
    for segment in rest {
        let display = provider.module_display_name(&resolved.module);
        let binding = lift_provider(provider.module_binding(&resolved.module, segment))?
            .ok_or_else(|| {
                E::Semantic(F::UnknownMember {
                    module: display,
                    module_site: resolved.site.clone(),
                    member: Arc::from(*segment),
                })
            })?;
        if !binding
            .defining_domain
            .is_visible_from(&accessing, binding.is_public)
        {
            return Err(E::Semantic(F::PrivateMember {
                member: Arc::from(*segment),
                site: binding.site,
                defining_file: binding.defining_file,
            }));
        }
        resolved = SemanticResolvedModule {
            module: binding.target,
            site: binding.site,
        };
    }
    Ok(resolved)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTypeConstructorParameter<N> {
    pub name: N,
    pub is_comptime: bool,
    pub is_type: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTypeConstructorHead<K, N, A> {
    pub key: K,
    pub site: A,
    pub parameters: Arc<[SemanticTypeConstructorParameter<N>]>,
    pub returns_type: bool,
    pub is_public: bool,
    pub defining_domain: SemanticVisibilityDomain,
    pub defining_file: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticComptimeCallResult<T, V> {
    Type(T),
    Value(V),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticComptimeCallExpectation {
    Type,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticResolvedComptimeCall<K, N, A, T, V> {
    pub head: SemanticTypeConstructorHead<K, N, A>,
    pub type_arguments: Vec<(N, T)>,
    pub value_arguments: Vec<(N, V)>,
    pub result: SemanticComptimeCallResult<T, V>,
}

pub type SemanticProviderResult<T, E, F> = Result<T, SemanticProviderError<E, F>>;

/// One comptime value argument preserved by structured type syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticValueSyntax<'a> {
    Integer(i128),
    Name(&'a str),
}

pub trait SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>:
    SemanticModulePathProvider<S, M, A>
{
    /// Run one structured-type poll with the exact substitution scope owned
    /// by its continuation. Ordinary providers already carry all relevant
    /// state in their own value, so the default keeps their behavior intact.
    /// Providers with ambient substitution maps may install and restore this
    /// scope around the canonical poll.
    fn with_comptime_substitutions<R>(
        &mut self,
        _type_substitutions: &[(N, T)],
        _value_substitutions: &[(N, V)],
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        operation(self)
    }

    fn substituted_type(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<Option<T>, Self::Abort, Self::Failure>;

    fn primitive_type(
        &mut self,
        name: &str,
    ) -> SemanticProviderResult<Option<T>, Self::Abort, Self::Failure>;

    fn builtin_type(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<Option<T>, Self::Abort, Self::Failure>;

    fn root_struct_type(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn root_enum_type(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn root_type_alias(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn module_struct_type(
        &mut self,
        module: &M,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn module_enum_type(
        &mut self,
        module: &M,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn module_type_alias(
        &mut self,
        module: &M,
        name: &str,
    ) -> SemanticProviderResult<Option<SemanticTypeFact<T, A>>, Self::Abort, Self::Failure>;

    fn observe_selected_named_type(
        &mut self,
        _name: &str,
        _kind: SemanticTypeFactKind,
        _fact: &SemanticTypeFact<T, A>,
    ) -> SemanticProviderResult<(), Self::Abort, Self::Failure> {
        Ok(())
    }

    fn observe_materialized_type(
        &mut self,
        _ty: &T,
    ) -> SemanticProviderResult<(), Self::Abort, Self::Failure> {
        Ok(())
    }

    fn allows_qualified_paths(&self, _scope: &S) -> bool {
        true
    }

    fn allows_qualified_comptime_call_head(
        &self,
        scope: &S,
        _expectation: SemanticComptimeCallExpectation,
    ) -> bool {
        self.allows_qualified_paths(scope)
    }

    fn resolve_array_length(
        &mut self,
        scope: &S,
        length: SemanticValueSyntax<'_>,
    ) -> SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure>;

    fn array_length_from_value(
        &mut self,
        scope: &S,
        value: &V,
    ) -> SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure>;

    fn array_type(
        &mut self,
        element: T,
        length: Option<u64>,
    ) -> SemanticProviderResult<T, Self::Abort, Self::Failure>;

    fn ptr_const_type(
        &mut self,
        pointee: T,
    ) -> SemanticProviderResult<T, Self::Abort, Self::Failure>;

    fn ptr_mut_type(&mut self, pointee: T)
    -> SemanticProviderResult<T, Self::Abort, Self::Failure>;

    fn slice_type(
        &mut self,
        scope: &S,
        syntax: &str,
        element: T,
    ) -> SemanticProviderResult<T, Self::Abort, Self::Failure>;

    fn builtin_type_call(
        &mut self,
        scope: &S,
        name: &str,
        arguments: &[SemanticValueSyntax<'_>],
    ) -> SemanticProviderResult<Option<T>, Self::Abort, Self::Failure>;

    fn root_constructor(
        &mut self,
        scope: &S,
        name: &str,
    ) -> SemanticProviderResult<
        Option<SemanticTypeConstructorHead<K, N, A>>,
        Self::Abort,
        Self::Failure,
    >;

    fn module_constructor(
        &mut self,
        module: &M,
        name: &str,
    ) -> SemanticProviderResult<
        Option<SemanticTypeConstructorHead<K, N, A>>,
        Self::Abort,
        Self::Failure,
    >;

    fn resolve_value_argument(
        &mut self,
        scope: &S,
        constructor: &str,
        head: &SemanticTypeConstructorHead<K, N, A>,
        parameter_index: usize,
        type_arguments: &[(N, T)],
        value_arguments: &[(N, V)],
        syntax: SemanticValueSyntax<'_>,
    ) -> SemanticProviderResult<V, Self::Abort, Self::Failure>;

    fn reduce_comptime_call(
        &mut self,
        head: &SemanticTypeConstructorHead<K, N, A>,
        type_arguments: &[(N, T)],
        value_arguments: &[(N, V)],
    ) -> SemanticProviderResult<Option<SemanticComptimeCallResult<T, V>>, Self::Abort, Self::Failure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticTypeSyntaxFailure<A, N> {
    Path(SemanticModulePathFailure<A>),
    UnknownType {
        syntax: Arc<str>,
    },
    UnknownConstructor {
        constructor: Arc<str>,
        expectation: SemanticComptimeCallExpectation,
    },
    UnknownModuleMember {
        module: Arc<str>,
        module_site: A,
        member: Arc<str>,
    },
    PrivateItem {
        kind: SemanticTypeFactKind,
        name: Arc<str>,
        site: A,
        defining_file: Arc<str>,
    },
    AmbiguousItem {
        name: Arc<str>,
        sites: Arc<[A]>,
    },
    NotTypeConstructor {
        constructor: Arc<str>,
        site: A,
    },
    TypeWhereValueExpected {
        constructor: Arc<str>,
        site: A,
    },
    InvalidConstructorArity {
        constructor: Arc<str>,
        site: A,
        expected: usize,
        found: usize,
        expectation: SemanticComptimeCallExpectation,
    },
    RuntimeConstructorParameter {
        constructor: Arc<str>,
        site: A,
        expected: usize,
        found: usize,
        expectation: SemanticComptimeCallExpectation,
    },
    ValueWhereTypeExpected {
        constructor: Arc<str>,
        site: A,
        argument: Arc<str>,
        parameter: N,
    },
    ConstructorDidNotReduce {
        constructor: Arc<str>,
        site: A,
    },
}

pub type SemanticTypeSyntaxError<E, P, A, N> =
    SemanticResolutionError<E, P, SemanticTypeSyntaxFailure<A, N>>;

fn visible_type<T, A, N, E, P>(
    fact: SemanticTypeFact<T, A>,
    kind: SemanticTypeFactKind,
    name: &str,
    accessing: &SemanticVisibilityDomain,
) -> Result<SemanticTypeFact<T, A>, SemanticTypeSyntaxError<E, P, A, N>> {
    if fact
        .defining_domain
        .is_visible_from(accessing, fact.is_public)
    {
        Ok(fact)
    } else {
        Err(SemanticResolutionError::Semantic(
            SemanticTypeSyntaxFailure::PrivateItem {
                kind,
                name: Arc::from(name),
                site: fact.site,
                defining_file: fact.defining_file,
            },
        ))
    }
}

fn select_named_type<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    fact: SemanticTypeFact<T, A>,
    kind: SemanticTypeFactKind,
    name: &str,
    accessing: &SemanticVisibilityDomain,
) -> Result<T, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    let fact = visible_type(fact, kind, name, accessing)?;
    lift_provider(provider.observe_selected_named_type(name, kind, &fact))?;
    if kind == SemanticTypeFactKind::Constant {
        lift_provider(provider.observe_materialized_type(&fact.value))?;
    }
    Ok(fact.value)
}

fn resolve_unqualified_semantic_type<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    name: &str,
) -> Result<Option<T>, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    // Computed lazily, next to the only readers below: the substituted,
    // primitive and builtin fast paths resolve most names and all return before
    // any visibility check, so eagerly deriving the domain here made every `i32`
    // pay a path parse and an Arc<str> allocation it discarded (RUE-1840).
    if let Some(ty) = lift_provider(provider.substituted_type(root_scope, name))? {
        lift_provider(provider.observe_materialized_type(&ty))?;
        return Ok(Some(ty));
    }
    if let Some(ty) = lift_provider(provider.primitive_type(name))? {
        return Ok(Some(ty));
    }
    if let Some(ty) = lift_provider(provider.builtin_type(root_scope, name))? {
        return Ok(Some(ty));
    }
    if let Some(fact) = lift_provider(provider.root_struct_type(root_scope, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Struct,
            name,
            &provider.accessing_domain(root_scope),
        )
        .map(Some);
    }
    if let Some(fact) = lift_provider(provider.root_enum_type(root_scope, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Enum,
            name,
            &provider.accessing_domain(root_scope),
        )
        .map(Some);
    }
    if let Some(fact) = lift_provider(provider.root_type_alias(root_scope, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Constant,
            name,
            &provider.accessing_domain(root_scope),
        )
        .map(Some);
    }
    Ok(None)
}

fn resolve_qualified_semantic_type<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    segments: &[&str],
    syntax: Arc<str>,
) -> Result<T, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    M: Clone,
    A: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    use SemanticResolutionError as E;
    use SemanticTypeSyntaxFailure as F;

    if !provider.allows_qualified_paths(root_scope) {
        return Err(E::Semantic(F::UnknownType { syntax }));
    }
    let Some((name, prefix)) = segments.split_last() else {
        return Err(E::Semantic(F::UnknownType { syntax }));
    };
    if prefix.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(E::Semantic(F::UnknownType { syntax }));
    }
    // Deferred past the module-path resolution below, which can fail, and past
    // the fact lookups: only a selected named type reads it (RUE-1840).
    let resolved = resolve_semantic_module_path(provider, root_scope, prefix)
        .map_err(|error| error.map_semantic(F::Path))?;
    if let Some(fact) = lift_provider(provider.module_struct_type(&resolved.module, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Struct,
            name,
            &provider.accessing_domain(root_scope),
        );
    }
    if let Some(fact) = lift_provider(provider.module_enum_type(&resolved.module, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Enum,
            name,
            &provider.accessing_domain(root_scope),
        );
    }
    if let Some(fact) = lift_provider(provider.module_type_alias(&resolved.module, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Constant,
            name,
            &provider.accessing_domain(root_scope),
        );
    }
    Err(E::Semantic(F::UnknownModuleMember {
        module: provider.module_display_name(&resolved.module),
        module_site: resolved.site,
        member: Arc::from(*name),
    }))
}

enum ResolvedComptimeArgument<T, V> {
    Type(T),
    Value(V),
}

/// Owned state for one comptime constructor call.  Admission is completed
/// before the first argument is resolved, and argument bindings are appended
/// strictly in parameter order.  Keeping this state separate from the
/// resolver closure makes it safe to suspend later without redoing path,
/// visibility, eligibility, or already-observed arguments.
struct SemanticComptimeCallState<K, N, A, T, V> {
    constructor: Arc<str>,
    expectation: SemanticComptimeCallExpectation,
    head: SemanticTypeConstructorHead<K, N, A>,
    type_arguments: Vec<(N, T)>,
    value_arguments: Vec<(N, V)>,
    next_parameter: usize,
}

struct SemanticComptimeCallRequest<K, N, A, T, V> {
    constructor: Arc<str>,
    expectation: SemanticComptimeCallExpectation,
    head: SemanticTypeConstructorHead<K, N, A>,
    type_arguments: Vec<(N, T)>,
    value_arguments: Vec<(N, V)>,
}

struct SemanticComptimeCallRequestView<'a, K, N, A, T, V> {
    head: &'a SemanticTypeConstructorHead<K, N, A>,
    type_arguments: &'a [(N, T)],
    value_arguments: &'a [(N, V)],
}

impl<'a, K, N, A, T, V> SemanticComptimeCallRequestView<'a, K, N, A, T, V> {
    fn head(&self) -> &SemanticTypeConstructorHead<K, N, A> {
        self.head
    }

    fn type_arguments(&self) -> &[(N, T)] {
        self.type_arguments
    }

    fn value_arguments(&self) -> &[(N, V)] {
        self.value_arguments
    }
}

impl<K, N, A, T, V> SemanticComptimeCallState<K, N, A, T, V>
where
    A: Clone,
    N: Clone,
{
    fn admit<S, M, P>(
        provider: &mut P,
        root_scope: &S,
        call_segments: &[&str],
        argument_count: usize,
        expectation: SemanticComptimeCallExpectation,
        call_display: impl FnOnce() -> Arc<str>,
    ) -> Result<Self, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
    where
        M: Clone,
        P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
    {
        use SemanticResolutionError as E;
        use SemanticTypeSyntaxFailure as F;

        if call_segments.is_empty()
            || call_segments.iter().any(|segment| segment.is_empty())
            || (call_segments.len() > 1
                && !provider.allows_qualified_comptime_call_head(root_scope, expectation))
        {
            return Err(E::Semantic(F::UnknownType {
                syntax: call_display(),
            }));
        }
        let constructor = Arc::<str>::from(call_segments.join("."));
        let name = *call_segments.last().expect("type call always has a name");
        let head = if call_segments.len() == 1 {
            lift_provider(provider.root_constructor(root_scope, name))?
        } else {
            let resolved = resolve_semantic_module_path(
                provider,
                root_scope,
                &call_segments[..call_segments.len() - 1],
            )
            .map_err(|error| error.map_semantic(F::Path))?;
            lift_provider(provider.module_constructor(&resolved.module, name))?
        }
        .ok_or_else(|| {
            E::Semantic(F::UnknownConstructor {
                constructor: constructor.clone(),
                expectation,
            })
        })?;
        let accessing = provider.accessing_domain(root_scope);
        if !head
            .defining_domain
            .is_visible_from(&accessing, head.is_public)
        {
            return Err(E::Semantic(F::PrivateItem {
                kind: SemanticTypeFactKind::Function,
                name: Arc::from(name),
                site: head.site,
                defining_file: head.defining_file,
            }));
        }
        if expectation == SemanticComptimeCallExpectation::Type && !head.returns_type {
            return Err(E::Semantic(F::NotTypeConstructor {
                constructor,
                site: head.site,
            }));
        }
        if expectation == SemanticComptimeCallExpectation::Value && head.returns_type {
            return Err(E::Semantic(F::TypeWhereValueExpected {
                constructor,
                site: head.site,
            }));
        }
        let eligible = head
            .parameters
            .iter()
            .all(|parameter| parameter.is_comptime)
            && (head.returns_type || !head.parameters.is_empty());
        if !eligible {
            return Err(E::Semantic(F::RuntimeConstructorParameter {
                constructor,
                site: head.site,
                expected: head.parameters.len(),
                found: argument_count,
                expectation,
            }));
        }
        if head.parameters.len() != argument_count {
            return Err(E::Semantic(F::InvalidConstructorArity {
                constructor,
                site: head.site,
                expected: head.parameters.len(),
                found: argument_count,
                expectation,
            }));
        }
        Ok(Self {
            constructor,
            expectation,
            head,
            type_arguments: Vec::new(),
            value_arguments: Vec::new(),
            next_parameter: 0,
        })
    }

    fn accept<E, F>(
        &mut self,
        argument_display: impl FnOnce() -> Arc<str>,
        resolved: Result<ResolvedComptimeArgument<T, V>, SemanticTypeSyntaxError<E, F, A, N>>,
    ) -> Result<(), SemanticTypeSyntaxError<E, F, A, N>> {
        use SemanticResolutionError as E2;
        use SemanticTypeSyntaxFailure as F2;
        let parameter_index = self.next_parameter;
        assert!(
            parameter_index < self.head.parameters.len(),
            "cannot accept an argument after a comptime call is complete"
        );
        let parameter = &self.head.parameters[parameter_index];
        match resolved {
            Ok(ResolvedComptimeArgument::Type(value)) if parameter.is_type => {
                self.type_arguments.push((parameter.name.clone(), value));
                self.next_parameter += 1;
                Ok(())
            }
            Ok(ResolvedComptimeArgument::Value(value)) if !parameter.is_type => {
                self.value_arguments.push((parameter.name.clone(), value));
                self.next_parameter += 1;
                Ok(())
            }
            Ok(ResolvedComptimeArgument::Value(_)) => {
                Err(E2::Semantic(F2::ValueWhereTypeExpected {
                    constructor: self.constructor.clone(),
                    site: self.head.site.clone(),
                    argument: argument_display(),
                    parameter: parameter.name.clone(),
                }))
            }
            Ok(ResolvedComptimeArgument::Type(_)) => {
                Err(E2::Semantic(F2::TypeWhereValueExpected {
                    constructor: self.constructor.clone(),
                    site: self.head.site.clone(),
                }))
            }
            Err(error) if parameter.is_type => Err(E2::ComptimeCallTypeArgument {
                constructor: self.constructor.clone(),
                argument_index: parameter_index,
                argument: argument_display(),
                error: Box::new(error),
            }),
            Err(error) => Err(error),
        }
    }

    fn into_request(self) -> SemanticComptimeCallRequest<K, N, A, T, V> {
        assert_eq!(
            self.next_parameter,
            self.head.parameters.len(),
            "cannot finish an incomplete comptime call"
        );
        SemanticComptimeCallRequest {
            constructor: self.constructor,
            expectation: self.expectation,
            head: self.head,
            type_arguments: self.type_arguments,
            value_arguments: self.value_arguments,
        }
    }
}

impl<K, N, A, T, V> SemanticComptimeCallRequest<K, N, A, T, V> {
    fn view(&self) -> SemanticComptimeCallRequestView<'_, K, N, A, T, V> {
        SemanticComptimeCallRequestView {
            head: &self.head,
            type_arguments: &self.type_arguments,
            value_arguments: &self.value_arguments,
        }
    }
}

impl<K, N, A: Clone, T, V> SemanticComptimeCallRequest<K, N, A, T, V> {
    fn complete<Abort, Failure>(
        self,
        reduced: SemanticProviderResult<Option<SemanticComptimeCallResult<T, V>>, Abort, Failure>,
    ) -> Result<
        SemanticResolvedComptimeCall<K, N, A, T, V>,
        SemanticTypeSyntaxError<Abort, Failure, A, N>,
    > {
        use SemanticResolutionError as E;
        use SemanticTypeSyntaxFailure as F;
        let constructor_site = self.head.site.clone();
        let result = lift_provider(reduced)?.ok_or_else(|| {
            E::Semantic(F::ConstructorDidNotReduce {
                constructor: self.constructor.clone(),
                site: constructor_site.clone(),
            })
        })?;
        if self.expectation == SemanticComptimeCallExpectation::Type
            && !matches!(result, SemanticComptimeCallResult::Type(_))
        {
            return Err(E::Semantic(F::ConstructorDidNotReduce {
                constructor: self.constructor,
                site: self.head.site,
            }));
        }
        Ok(SemanticResolvedComptimeCall {
            head: self.head,
            type_arguments: self.type_arguments,
            value_arguments: self.value_arguments,
            result,
        })
    }
}

fn structured_path<'a, Sym>(
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    range: rue_rir::RirTypeSyntaxRange,
    resolve_symbol: impl Copy + Fn(&'a Sym) -> &'a str,
) -> Option<Vec<&'a str>> {
    arena
        .words(range)?
        .iter()
        .map(|word| {
            arena
                .symbol(rue_rir::RirTypeSyntaxSymbol::from_u32(*word))
                .map(resolve_symbol)
        })
        .collect()
}

fn structured_references<Sym>(
    arena: &rue_rir::RirTypeSyntaxArena<Sym>,
    range: rue_rir::RirTypeSyntaxRange,
) -> Option<Vec<rue_rir::RirTypeSyntaxRef>> {
    Some(
        arena
            .words(range)?
            .iter()
            .copied()
            .map(rue_rir::RirTypeSyntaxRef::from_u32)
            .collect(),
    )
}

fn structured_syntax_display<'a, Sym>(
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    reference: rue_rir::RirTypeSyntaxRef,
    resolve_symbol: impl Copy + Fn(&'a Sym) -> &'a str,
) -> Arc<str> {
    arena
        .render_type_with(reference, resolve_symbol)
        .map(Arc::from)
        .unwrap_or_else(|| Arc::from("<invalid structured type syntax>"))
}

fn structured_type_diagnostic_display<'a, Sym>(
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    reference: rue_rir::RirTypeSyntaxRef,
    resolve_symbol: impl Copy + Fn(&'a Sym) -> &'a str,
) -> Arc<str> {
    match arena.node(reference) {
        Some(rue_rir::RirTypeSyntaxNode::TypeCall { path, .. }) => {
            let Some(segments) = structured_path(arena, *path, resolve_symbol) else {
                return Arc::from("<invalid structured type syntax>");
            };
            Arc::from(format!("{}(...)", segments.join(".")))
        }
        Some(rue_rir::RirTypeSyntaxNode::ValueCall { name, .. }) => arena
            .symbol(*name)
            .map(|symbol| Arc::from(format!("{}(...)", resolve_symbol(symbol))))
            .unwrap_or_else(|| Arc::from("<invalid structured type syntax>")),
        _ => structured_syntax_display(arena, reference, resolve_symbol),
    }
}

fn structured_value_syntax<'a, Sym>(
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    reference: rue_rir::RirTypeSyntaxRef,
    resolve_symbol: impl Copy + Fn(&'a Sym) -> &'a str,
) -> Option<SemanticValueSyntax<'a>> {
    match arena.node(reference)? {
        rue_rir::RirTypeSyntaxNode::Integer(value) => Some(SemanticValueSyntax::Integer(*value)),
        rue_rir::RirTypeSyntaxNode::Named(symbol) => arena
            .symbol(*symbol)
            .map(|symbol| SemanticValueSyntax::Name(resolve_symbol(symbol))),
        _ => None,
    }
}

enum StructuredCallDestination<T> {
    Root {
        reference: rue_rir::RirTypeSyntaxRef,
    },
    Argument {
        parent: usize,
    },
    ArrayLength {
        reference: rue_rir::RirTypeSyntaxRef,
        element: T,
    },
}

struct StructuredCallFrame<K, N, A, T, V> {
    state: SemanticComptimeCallState<K, N, A, T, V>,
    arguments: Vec<rue_rir::RirTypeSyntaxRef>,
    destination: StructuredCallDestination<T>,
}

enum StructuredTypePoll<K, N, A, T, V> {
    Ready(T),
    Suspended(Box<StructuredTypeSuspension<K, N, A, T, V>>),
}

struct StructuredTypeSuspendedCall<K, N, A, T, V> {
    request: SemanticComptimeCallRequest<K, N, A, T, V>,
    destination: StructuredCallDestination<T>,
}

struct StructuredTypeSuspension<K, N, A, T, V> {
    machine: StructuredTypeMachine<K, N, A, T, V>,
    call: StructuredTypeSuspendedCall<K, N, A, T, V>,
}

impl<K, N, A, T, V> StructuredTypeSuspension<K, N, A, T, V> {
    fn request(&self) -> SemanticComptimeCallRequestView<'_, K, N, A, T, V> {
        self.call.request.view()
    }
}

/// The completed call request exposed to the comptime orchestration host.
///
/// The program identity is deliberately carried beside the request rather
/// than inferred from any syntax index. Syntax references and symbol indices
/// are local to the owned arena and are not an identity mechanism.
#[derive(Debug, Clone, Copy)]
pub struct ComptimeStructuredTypeRequest<'a, P, C, N, A, T, V> {
    program: &'a P,
    head: &'a SemanticTypeConstructorHead<C, N, A>,
    type_arguments: &'a [(N, T)],
    value_arguments: &'a [(N, V)],
}

/// Owned authority admitted from one registered program snapshot. The arena,
/// symbol spelling table, root scope, and program identity travel as one
/// value, so a continuation cannot be paired with a caller-selected arena.
pub struct ComptimeStructuredTypeAuthority<P, S, Sym, R> {
    program: P,
    root_scope: S,
    arena: rue_rir::RirTypeSyntaxArena<Sym>,
    symbols: R,
    root: rue_rir::RirTypeSyntaxRef,
}

pub type RegisteredComptimeStructuredTypeAuthority<D, C, Scope, S> =
    ComptimeStructuredTypeAuthority<crate::ComptimeProgramKey<D, C>, Scope, Spur, Arc<[S]>>;

/// A structured authority using the registry's canonical syntax and symbol
/// domains while carrying a caller-owned identity for the continuation.
pub type ComptimeStructuredTypeAuthorityWithProgram<P, Scope, S> =
    ComptimeStructuredTypeAuthority<P, Scope, Spur, Arc<[S]>>;

pub trait ComptimeStructuredTypeSymbolAuthority<Sym> {
    fn resolve_symbol<'a>(&'a self, symbol: &'a Sym) -> Option<&'a str>;
}

pub(crate) fn registered_symbol_authority_is_valid<S: AsRef<str>>(
    arena: &rue_rir::RirTypeSyntaxArena<Spur>,
    symbols: &[S],
) -> bool {
    arena
        .symbols()
        .iter()
        .all(|symbol| symbols.get(symbol.into_usize()).is_some())
}

impl<S: AsRef<str>> ComptimeStructuredTypeSymbolAuthority<Spur> for Arc<[S]> {
    fn resolve_symbol<'a>(&'a self, symbol: &'a Spur) -> Option<&'a str> {
        self.get(symbol.into_usize()).map(AsRef::as_ref)
    }
}

impl ComptimeStructuredTypeSymbolAuthority<Arc<str>> for Arc<[Arc<str>]> {
    fn resolve_symbol<'a>(&'a self, symbol: &'a Arc<str>) -> Option<&'a str> {
        Some(symbol.as_ref())
    }
}

impl<P, S, Sym, R> ComptimeStructuredTypeAuthority<P, S, Sym, R> {
    pub fn program(&self) -> &P {
        &self.program
    }

    pub(crate) fn from_registered(
        program: P,
        root_scope: S,
        arena: rue_rir::RirTypeSyntaxArena<Sym>,
        symbols: R,
        root: rue_rir::RirTypeSyntaxRef,
    ) -> Self {
        Self {
            program,
            root_scope,
            arena,
            symbols,
            root,
        }
    }
}

impl<'a, P, C, N, A, T, V> ComptimeStructuredTypeRequest<'a, P, C, N, A, T, V> {
    pub fn program(&self) -> &'a P {
        self.program
    }

    pub fn head(&self) -> &'a SemanticTypeConstructorHead<C, N, A> {
        self.head
    }

    pub fn type_arguments(&self) -> &'a [(N, T)] {
        self.type_arguments
    }

    pub fn value_arguments(&self) -> &'a [(N, V)] {
        self.value_arguments
    }
}

/// One opaque, keyed structured-type job.
///
/// All state needed to continue a type-syntax reduction is owned here. In
/// particular, the arena and root scope cannot be replaced by a caller when
/// [`ComptimeStructuredTypeJob::resume`] is called. This prevents a dense
/// `RirTypeSyntaxRef` from being interpreted against another program's local
/// arena, even when the two programs happen to use colliding indices.
pub struct ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R> {
    // These scopes travel with the consuming continuation. A resumed poll
    // therefore cannot accidentally observe another call's ambient maps.
    authority: ComptimeStructuredTypeAuthority<P, S, Sym, R>,
    suspension: StructuredTypeSuspension<C, N, A, T, V>,
    type_substitutions: Vec<(N, T)>,
    value_substitutions: Vec<(N, V)>,
}

impl<P, S, C, N, A, T, V, Sym, R> ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R>
where
    R: ComptimeStructuredTypeSymbolAuthority<Sym>,
{
    fn request(&self) -> ComptimeStructuredTypeRequest<'_, P, C, N, A, T, V> {
        let SemanticComptimeCallRequestView {
            head,
            type_arguments,
            value_arguments,
        } = self.suspension.request();
        ComptimeStructuredTypeRequest {
            program: &self.authority.program,
            head,
            type_arguments,
            value_arguments,
        }
    }

    pub fn program(&self) -> &P {
        &self.authority.program
    }

    pub fn request_view(&self) -> ComptimeStructuredTypeRequest<'_, P, C, N, A, T, V> {
        self.request()
    }

    pub fn head(&self) -> &SemanticTypeConstructorHead<C, N, A> {
        let SemanticComptimeCallRequestView { head, .. } = self.suspension.request();
        head
    }

    pub fn type_arguments(&self) -> &[(N, T)] {
        let SemanticComptimeCallRequestView { type_arguments, .. } = self.suspension.request();
        type_arguments
    }

    pub fn value_arguments(&self) -> &[(N, V)] {
        let SemanticComptimeCallRequestView {
            value_arguments, ..
        } = self.suspension.request();
        value_arguments
    }
}

/// Result of polling the canonical structured-type machine through a keyed
/// job. The job variant is consuming: a suspended computation has exactly one
/// continuation and is never cloneable or replayable.
pub enum ComptimeStructuredTypePoll<P, S, C, N, A, T, V, Sym, R> {
    Ready(T),
    Suspended(Box<ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R>>),
}

impl<P, S, C, N, A, T, V, Sym, R> ComptimeStructuredTypeJob<P, S, C, N, A, T, V, Sym, R>
where
    R: ComptimeStructuredTypeSymbolAuthority<Sym>,
{
    fn from_suspension(
        authority: ComptimeStructuredTypeAuthority<P, S, Sym, R>,
        suspension: StructuredTypeSuspension<C, N, A, T, V>,
        type_substitutions: Vec<(N, T)>,
        value_substitutions: Vec<(N, V)>,
    ) -> Self {
        Self {
            authority,
            suspension,
            type_substitutions,
            value_substitutions,
        }
    }

    /// Start a keyed job using an arena whose symbol authority is owned by
    /// the job. The first suspension is produced by the canonical structured
    /// machine; there is no alternate traversal for this seam.
    pub fn begin<M, Q>(
        provider: &mut Q,
        authority: ComptimeStructuredTypeAuthority<P, S, Sym, R>,
        type_substitutions: Vec<(N, T)>,
        value_substitutions: Vec<(N, V)>,
    ) -> Result<
        ComptimeStructuredTypePoll<P, S, C, N, A, T, V, Sym, R>,
        SemanticTypeSyntaxError<Q::Abort, Q::Failure, A, N>,
    >
    where
        M: Clone,
        A: Clone,
        N: Clone,
        Q: SemanticTypeSyntaxProvider<S, M, A, C, N, T, V>,
        R: ComptimeStructuredTypeSymbolAuthority<Sym>,
    {
        let ComptimeStructuredTypeAuthority {
            program,
            root_scope,
            arena,
            symbols,
            root,
        } = authority;
        let poll = provider.with_comptime_substitutions(
            &type_substitutions,
            &value_substitutions,
            |provider| {
                poll_structured_type_machine(
                    StructuredTypeMachine::<C, N, A, T, V>::new(root),
                    provider,
                    &root_scope,
                    &arena,
                    |symbol: &Sym| {
                        symbols
                            .resolve_symbol(symbol)
                            .expect("structured type authority was admitted with all symbols")
                    },
                )
            },
        )?;
        Ok(match poll {
            StructuredTypePoll::Ready(value) => ComptimeStructuredTypePoll::Ready(value),
            StructuredTypePoll::Suspended(suspension) => {
                ComptimeStructuredTypePoll::Suspended(Box::new(Self::from_suspension(
                    ComptimeStructuredTypeAuthority::from_registered(
                        program, root_scope, arena, symbols, root,
                    ),
                    *suspension,
                    type_substitutions,
                    value_substitutions,
                )))
            }
        })
    }

    /// Resume this job with the reduction selected by the host. The caller
    /// supplies neither a program identity nor any syntax authority: both are
    /// part of this consuming continuation.
    pub fn resume<M, Q>(
        self,
        provider: &mut Q,
        reduced: SemanticProviderResult<
            Option<SemanticComptimeCallResult<T, V>>,
            Q::Abort,
            Q::Failure,
        >,
    ) -> Result<
        ComptimeStructuredTypePoll<P, S, C, N, A, T, V, Sym, R>,
        SemanticTypeSyntaxError<Q::Abort, Q::Failure, A, N>,
    >
    where
        M: Clone,
        A: Clone,
        N: Clone,
        Q: SemanticTypeSyntaxProvider<S, M, A, C, N, T, V>,
        R: ComptimeStructuredTypeSymbolAuthority<Sym>,
    {
        let Self {
            authority,
            suspension,
            type_substitutions,
            value_substitutions,
        } = self;
        let ComptimeStructuredTypeAuthority {
            program,
            root_scope,
            arena,
            symbols,
            root,
        } = authority;
        let poll = provider.with_comptime_substitutions(
            &type_substitutions,
            &value_substitutions,
            |provider| {
                suspension.resume(
                    provider,
                    &root_scope,
                    &arena,
                    |symbol: &Sym| {
                        symbols
                            .resolve_symbol(symbol)
                            .expect("structured type authority was admitted with all symbols")
                    },
                    reduced,
                )
            },
        )?;
        Ok(match poll {
            StructuredTypePoll::Ready(value) => ComptimeStructuredTypePoll::Ready(value),
            StructuredTypePoll::Suspended(suspension) => {
                ComptimeStructuredTypePoll::Suspended(Box::new(Self::from_suspension(
                    ComptimeStructuredTypeAuthority::from_registered(
                        program, root_scope, arena, symbols, root,
                    ),
                    *suspension,
                    type_substitutions,
                    value_substitutions,
                )))
            }
        })
    }
}

/// Resolve one parser-structured type without reconstructing or tokenizing its
/// source spelling.
pub fn resolve_structured_semantic_type_syntax<S, Sym, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    arena: &rue_rir::RirTypeSyntaxArena<Sym>,
    root: rue_rir::RirTypeSyntaxRef,
) -> Result<T, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    Sym: AsRef<str>,
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    resolve_structured_semantic_type_syntax_with(provider, root_scope, arena, root, AsRef::as_ref)
}

enum StructuredTypeWork<T> {
    Evaluate(rue_rir::RirTypeSyntaxRef),
    FinishArray {
        reference: rue_rir::RirTypeSyntaxRef,
        length: rue_rir::RirTypeSyntaxRef,
    },
    FinishSlice {
        syntax: Arc<str>,
    },
    FinishPointerConst,
    FinishPointerMut,
    BeginCall {
        reference: rue_rir::RirTypeSyntaxRef,
        segments: Vec<Arc<str>>,
        arguments: Vec<rue_rir::RirTypeSyntaxRef>,
        expectation: SemanticComptimeCallExpectation,
        destination: StructuredCallDestination<T>,
    },
    DriveCall(usize),
    AcceptCallValue {
        parent: usize,
    },
    CatchTypeArgument {
        parent: usize,
        argument: rue_rir::RirTypeSyntaxRef,
    },
    FinishCall(usize),
}

/// Resolve a structured type whose symbols are owned by a separate authority,
/// such as a body-local RIR interner. The resolver is consulted directly; no
/// spelling is reconstructed and reparsed.
struct StructuredTypeMachine<K, N, A, T, V> {
    root: rue_rir::RirTypeSyntaxRef,
    work: Vec<StructuredTypeWork<T>>,
    values: Vec<T>,
    calls: Vec<StructuredCallFrame<K, N, A, T, V>>,
}

impl<K, N, A, T, V> StructuredTypeMachine<K, N, A, T, V> {
    fn new(root: rue_rir::RirTypeSyntaxRef) -> Self {
        Self {
            root,
            work: vec![StructuredTypeWork::Evaluate(root)],
            values: Vec::new(),
            calls: Vec::new(),
        }
    }
}

pub fn resolve_structured_semantic_type_syntax_with<'a, S, Sym, M, A, K, N, T, V, P, R>(
    provider: &mut P,
    root_scope: &S,
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    root: rue_rir::RirTypeSyntaxRef,
    resolve_symbol: R,
) -> Result<T, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    R: Copy + Fn(&'a Sym) -> &'a str,
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    let mut poll = poll_structured_type_machine(
        StructuredTypeMachine::<K, N, A, T, V>::new(root),
        provider,
        root_scope,
        arena,
        resolve_symbol,
    )?;
    loop {
        match poll {
            StructuredTypePoll::Ready(value) => return Ok(value),
            StructuredTypePoll::Suspended(suspension) => {
                let suspension = *suspension;
                let request = suspension.request();
                let reduced = provider.reduce_comptime_call(
                    request.head(),
                    request.type_arguments(),
                    request.value_arguments(),
                );
                poll = suspension.resume(provider, root_scope, arena, resolve_symbol, reduced)?;
            }
        }
    }
}

fn poll_structured_type_machine<'a, S, Sym, M, A, K, N, T, V, P, R>(
    mut machine: StructuredTypeMachine<K, N, A, T, V>,
    provider: &mut P,
    root_scope: &S,
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    resolve_symbol: R,
) -> Result<StructuredTypePoll<K, N, A, T, V>, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    R: Copy + Fn(&'a Sym) -> &'a str,
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    use SemanticResolutionError as E;
    use SemanticTypeSyntaxFailure as F;
    use rue_rir::RirTypeSyntaxNode as R;

    let unknown = |reference| {
        E::Semantic(F::UnknownType {
            syntax: structured_type_diagnostic_display(arena, reference, resolve_symbol),
        })
    };
    loop {
        let Some(item) = machine.work.pop() else {
            return match machine.values.pop() {
                Some(value) if machine.values.is_empty() => Ok(StructuredTypePoll::Ready(value)),
                _ => Err(unknown(machine.root)),
            };
        };

        let mut machine_slot = Some(machine);
        let step: Result<
            Option<StructuredTypePoll<K, N, A, T, V>>,
            SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
        > = {
            let machine_slot = &mut machine_slot;
            (|| {
                let machine = machine_slot.as_mut().expect("live type machine");
                match item {
                    StructuredTypeWork::Evaluate(reference) => {
                        let node_unknown = || unknown(reference);
                        let node = arena.node(reference).cloned().ok_or_else(node_unknown)?;
                        match node {
                            R::Named(symbol) => {
                                let name =
                                    resolve_symbol(arena.symbol(symbol).ok_or_else(node_unknown)?);
                                machine.values.push(
                                    resolve_unqualified_semantic_type(provider, root_scope, name)?
                                        .ok_or_else(node_unknown)?,
                                );
                            }
                            R::Qualified { path } => {
                                let segments = structured_path(arena, path, resolve_symbol)
                                    .ok_or_else(node_unknown)?;
                                machine.values.push(resolve_qualified_semantic_type(
                                    provider,
                                    root_scope,
                                    &segments,
                                    structured_syntax_display(arena, reference, resolve_symbol),
                                )?);
                            }
                            R::Unit => machine.values.push(
                                resolve_unqualified_semantic_type(provider, root_scope, "()")?
                                    .ok_or_else(node_unknown)?,
                            ),
                            R::Never => machine.values.push(
                                resolve_unqualified_semantic_type(provider, root_scope, "!")?
                                    .ok_or_else(node_unknown)?,
                            ),
                            R::Array { element, length } => {
                                machine
                                    .work
                                    .push(StructuredTypeWork::FinishArray { reference, length });
                                machine.work.push(StructuredTypeWork::Evaluate(element));
                            }
                            R::Slice { element } => {
                                machine.work.push(StructuredTypeWork::FinishSlice {
                                    syntax: structured_syntax_display(
                                        arena,
                                        reference,
                                        resolve_symbol,
                                    ),
                                });
                                machine.work.push(StructuredTypeWork::Evaluate(element));
                            }
                            R::PointerConst { pointee } => {
                                machine.work.push(StructuredTypeWork::FinishPointerConst);
                                machine.work.push(StructuredTypeWork::Evaluate(pointee));
                            }
                            R::PointerMut { pointee } => {
                                machine.work.push(StructuredTypeWork::FinishPointerMut);
                                machine.work.push(StructuredTypeWork::Evaluate(pointee));
                            }
                            R::TypeCall { path, arguments } => {
                                let segments = structured_path(arena, path, resolve_symbol)
                                    .ok_or_else(node_unknown)?;
                                let arguments = structured_references(arena, arguments)
                                    .ok_or_else(node_unknown)?;
                                if let [name] = segments.as_slice()
                                    && let Some(value_arguments) = arguments
                                        .iter()
                                        .copied()
                                        .map(|argument| {
                                            structured_value_syntax(arena, argument, resolve_symbol)
                                        })
                                        .collect::<Option<Vec<_>>>()
                                    && let Some(ty) = lift_provider(provider.builtin_type_call(
                                        root_scope,
                                        name,
                                        &value_arguments,
                                    ))?
                                {
                                    machine.values.push(ty);
                                } else {
                                    machine.work.push(StructuredTypeWork::BeginCall {
                                        reference,
                                        segments: segments.into_iter().map(Arc::from).collect(),
                                        arguments,
                                        expectation: SemanticComptimeCallExpectation::Type,
                                        destination: StructuredCallDestination::Root { reference },
                                    });
                                }
                            }
                            R::AnonymousStruct { .. }
                            | R::AnonymousEnum { .. }
                            | R::ValueCall { .. }
                            | R::Integer(_) => return Err(node_unknown()),
                        }
                    }
                    StructuredTypeWork::BeginCall {
                        reference,
                        segments,
                        arguments,
                        expectation,
                        destination,
                    } => {
                        let segment_refs: Vec<&str> = segments.iter().map(AsRef::as_ref).collect();
                        let state = SemanticComptimeCallState::admit(
                            provider,
                            root_scope,
                            &segment_refs,
                            arguments.len(),
                            expectation,
                            || structured_type_diagnostic_display(arena, reference, resolve_symbol),
                        )?;
                        let frame_index = machine.calls.len();
                        machine.calls.push(StructuredCallFrame {
                            state,
                            arguments,
                            destination,
                        });
                        machine
                            .work
                            .push(StructuredTypeWork::DriveCall(frame_index));
                    }
                    StructuredTypeWork::DriveCall(frame_index) => {
                        let frame = machine.calls.get(frame_index).expect("live call frame");
                        if frame.state.next_parameter == frame.arguments.len() {
                            machine
                                .work
                                .push(StructuredTypeWork::FinishCall(frame_index));
                            return Ok(None);
                        }
                        let index = frame.state.next_parameter;
                        let parameter = frame.state.head.parameters[index].clone();
                        let argument = frame.arguments[index];
                        if parameter.is_type {
                            if matches!(arena.node(argument), Some(R::Integer(_))) {
                                return Err(E::Semantic(F::ValueWhereTypeExpected {
                                    constructor: frame.state.constructor.clone(),
                                    site: frame.state.head.site.clone(),
                                    argument: structured_syntax_display(
                                        arena,
                                        argument,
                                        resolve_symbol,
                                    ),
                                    parameter: parameter.name,
                                }));
                            }
                            machine.work.push(StructuredTypeWork::CatchTypeArgument {
                                parent: frame_index,
                                argument,
                            });
                            machine.work.push(StructuredTypeWork::AcceptCallValue {
                                parent: frame_index,
                            });
                            machine.work.push(StructuredTypeWork::Evaluate(argument));
                            return Ok(None);
                        }
                        if let Some(syntax) =
                            structured_value_syntax(arena, argument, resolve_symbol)
                        {
                            let frame = machine.calls.get(frame_index).expect("live call frame");
                            let resolved = lift_provider(provider.resolve_value_argument(
                                root_scope,
                                &frame.state.constructor,
                                &frame.state.head,
                                index,
                                &frame.state.type_arguments,
                                &frame.state.value_arguments,
                                syntax,
                            ))
                            .map(ResolvedComptimeArgument::Value);
                            let frame =
                                machine.calls.get_mut(frame_index).expect("live call frame");
                            frame.state.accept(
                                || structured_syntax_display(arena, argument, resolve_symbol),
                                resolved,
                            )?;
                            machine
                                .work
                                .push(StructuredTypeWork::DriveCall(frame_index));
                            return Ok(None);
                        }
                        let call = match arena.node(argument) {
                            Some(R::TypeCall { path, arguments }) => Some((
                                structured_path(arena, *path, resolve_symbol),
                                structured_references(arena, *arguments),
                            )),
                            Some(R::ValueCall { name, arguments }) => Some((
                                arena.symbol(*name).map(|name| vec![resolve_symbol(name)]),
                                structured_references(arena, *arguments),
                            )),
                            _ => None,
                        };
                        if let Some((Some(segments), Some(arguments))) = call {
                            machine.work.push(StructuredTypeWork::BeginCall {
                                reference: argument,
                                segments: segments.into_iter().map(Arc::from).collect(),
                                arguments,
                                expectation: SemanticComptimeCallExpectation::Value,
                                destination: StructuredCallDestination::Argument {
                                    parent: frame_index,
                                },
                            });
                            return Ok(None);
                        } else {
                            machine.work.push(StructuredTypeWork::AcceptCallValue {
                                parent: frame_index,
                            });
                            machine.work.push(StructuredTypeWork::Evaluate(argument));
                        }
                    }
                    StructuredTypeWork::AcceptCallValue { parent } => {
                        let resolved = match machine.values.pop() {
                            Some(value) => ResolvedComptimeArgument::Type(value),
                            None => return Err(unknown(machine.root)),
                        };
                        let frame = machine.calls.get_mut(parent).expect("live call frame");
                        let is_type =
                            frame.state.head.parameters[frame.state.next_parameter].is_type;
                        let argument = frame.arguments[frame.state.next_parameter];
                        frame.state.accept(
                            || structured_syntax_display(arena, argument, resolve_symbol),
                            Ok(resolved),
                        )?;
                        if is_type {
                            match machine.work.pop() {
                                Some(StructuredTypeWork::CatchTypeArgument {
                                    parent: p, ..
                                }) if p == parent => {}
                                _ => panic!("type argument boundary missing"),
                            }
                        }
                        machine.work.push(StructuredTypeWork::DriveCall(parent));
                    }
                    StructuredTypeWork::CatchTypeArgument { .. } => {
                        panic!("type argument boundary must be consumed by its result");
                    }
                    StructuredTypeWork::FinishCall(frame_index) => {
                        let frame = machine.calls.pop().expect("call frames are LIFO");
                        assert_eq!(frame_index, machine.calls.len());
                        let StructuredCallFrame {
                            state, destination, ..
                        } = frame;
                        let machine = machine_slot.take().expect("live type machine");
                        return Ok(Some(StructuredTypePoll::Suspended(Box::new(
                            StructuredTypeSuspension {
                                machine,
                                call: StructuredTypeSuspendedCall {
                                    request: state.into_request(),
                                    destination,
                                },
                            },
                        ))));
                    }
                    StructuredTypeWork::FinishArray { reference, length } => {
                        let Some(element) = machine.values.pop() else {
                            return Err(unknown(reference));
                        };
                        if let Some(syntax) = structured_value_syntax(arena, length, resolve_symbol)
                        {
                            let length =
                                lift_provider(provider.resolve_array_length(root_scope, syntax))?;
                            machine
                                .values
                                .push(lift_provider(provider.array_type(element, length))?);
                        } else if let Some(R::ValueCall { name, arguments }) =
                            arena.node(length).cloned()
                        {
                            let name = resolve_symbol(
                                arena.symbol(name).ok_or_else(|| unknown(reference))?,
                            );
                            let arguments = structured_references(arena, arguments)
                                .ok_or_else(|| unknown(reference))?;
                            machine.work.push(StructuredTypeWork::BeginCall {
                                reference: length,
                                segments: vec![Arc::from(name)],
                                arguments,
                                expectation: SemanticComptimeCallExpectation::Value,
                                destination: StructuredCallDestination::ArrayLength {
                                    reference,
                                    element,
                                },
                            });
                        } else {
                            return Err(unknown(reference));
                        }
                    }
                    StructuredTypeWork::FinishSlice { syntax } => {
                        let Some(element) = machine.values.pop() else {
                            return Err(unknown(machine.root));
                        };
                        machine.values.push(lift_provider(
                            provider.slice_type(root_scope, &syntax, element),
                        )?);
                    }
                    StructuredTypeWork::FinishPointerConst => {
                        let Some(pointee) = machine.values.pop() else {
                            return Err(unknown(machine.root));
                        };
                        machine
                            .values
                            .push(lift_provider(provider.ptr_const_type(pointee))?);
                    }
                    StructuredTypeWork::FinishPointerMut => {
                        let Some(pointee) = machine.values.pop() else {
                            return Err(unknown(machine.root));
                        };
                        machine
                            .values
                            .push(lift_provider(provider.ptr_mut_type(pointee))?);
                    }
                }
                Ok(None)
            })()
        };

        match step {
            Ok(Some(poll)) => return Ok(poll),
            Ok(None) => machine = machine_slot.take().expect("live type machine"),
            Err(error) => {
                machine = machine_slot.take().expect("live type machine");
                unwind_structured_type_error(&mut machine, error, arena, resolve_symbol)?;
            }
        }
    }
}

fn unwind_structured_type_error<'a, Sym, K, N, A, T, V, E, F, R>(
    machine: &mut StructuredTypeMachine<K, N, A, T, V>,
    mut error: SemanticTypeSyntaxError<E, F, A, N>,
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    resolve_symbol: R,
) -> Result<(), SemanticTypeSyntaxError<E, F, A, N>>
where
    R: Copy + Fn(&'a Sym) -> &'a str,
    A: Clone,
    N: Clone,
{
    loop {
        let Some(item) = machine.work.pop() else {
            return Err(error);
        };
        let StructuredTypeWork::CatchTypeArgument { parent, argument } = item else {
            continue;
        };
        let frame = machine.calls.get_mut(parent).expect("parent call frame");
        let wrapped = frame.state.accept(
            || structured_syntax_display(arena, argument, resolve_symbol),
            Err(error),
        );
        let Err(next) = wrapped else {
            unreachable!("a type-argument boundary cannot recover from an error")
        };
        error = next;
    }
}

impl<K, N, A, T, V> StructuredTypeSuspension<K, N, A, T, V> {
    fn resume<'a, S, Sym, M, P, R>(
        self,
        provider: &mut P,
        root_scope: &S,
        arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
        resolve_symbol: R,
        reduced: SemanticProviderResult<
            Option<SemanticComptimeCallResult<T, V>>,
            P::Abort,
            P::Failure,
        >,
    ) -> Result<
        StructuredTypePoll<K, N, A, T, V>,
        SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
    >
    where
        R: Copy + Fn(&'a Sym) -> &'a str,
        M: Clone,
        A: Clone,
        N: Clone,
        P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
    {
        use SemanticResolutionError as E;
        use SemanticTypeSyntaxFailure as F;
        let StructuredTypeSuspension { mut machine, call } = self;
        let StructuredTypeSuspendedCall {
            request,
            destination,
        } = call;
        let routed = (|| {
            let resolved = request.complete(reduced)?;
            match destination {
                StructuredCallDestination::Root { reference } => {
                    let SemanticComptimeCallResult::Type(value) = resolved.result else {
                        return Err(E::Semantic(F::UnknownType {
                            syntax: structured_type_diagnostic_display(
                                arena,
                                reference,
                                resolve_symbol,
                            ),
                        }));
                    };
                    lift_provider(provider.observe_materialized_type(&value))?;
                    machine.values.push(value);
                }
                StructuredCallDestination::Argument { parent } => {
                    let value = match resolved.result {
                        SemanticComptimeCallResult::Type(value) => {
                            ResolvedComptimeArgument::Type(value)
                        }
                        SemanticComptimeCallResult::Value(value) => {
                            ResolvedComptimeArgument::Value(value)
                        }
                    };
                    let frame = machine.calls.get_mut(parent).expect("parent call frame");
                    let argument = frame.arguments[frame.state.next_parameter];
                    frame.state.accept(
                        || structured_syntax_display(arena, argument, resolve_symbol),
                        Ok(value),
                    )?;
                    machine.work.push(StructuredTypeWork::DriveCall(parent));
                }
                StructuredCallDestination::ArrayLength { reference, element } => {
                    let SemanticComptimeCallResult::Value(value) = resolved.result else {
                        return Err(E::Semantic(F::UnknownType {
                            syntax: structured_type_diagnostic_display(
                                arena,
                                reference,
                                resolve_symbol,
                            ),
                        }));
                    };
                    let length =
                        lift_provider(provider.array_length_from_value(root_scope, &value))?;
                    machine
                        .values
                        .push(lift_provider(provider.array_type(element, length))?);
                }
            }
            Ok(())
        })();
        if let Err(error) = routed {
            unwind_structured_type_error(&mut machine, error, arena, resolve_symbol)?;
        }
        poll_structured_type_machine(machine, provider, root_scope, arena, resolve_symbol)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lasso::ThreadedRodeo;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_parser::ast::Item;

    use super::*;

    type Fact = SemanticTypeFact<&'static str, &'static str>;
    type Binding = SemanticModuleBinding<&'static str, &'static str>;
    type Head = SemanticTypeConstructorHead<&'static str, &'static str, &'static str>;
    type FixtureResult<T> = SemanticProviderResult<T, &'static str, &'static str>;

    #[derive(Default)]
    struct Fixture {
        calls: Vec<String>,
        /// Times `accessing_domain` was derived. RUE-1840 guards that the
        /// fast paths never ask for it; a `Cell` because the hook takes
        /// `&self` while the `calls` trace needs `&mut self`.
        accessing_domain_calls: std::cell::Cell<usize>,
        reduced_arguments: Vec<String>,
        active_type_substitutions: BTreeMap<&'static str, &'static str>,
        active_value_substitutions: BTreeMap<&'static str, i64>,
        scope_observations: Vec<(Vec<(&'static str, &'static str)>, Vec<(&'static str, i64)>)>,
        bindings: BTreeMap<(&'static str, &'static str), Binding>,
        root_structs: BTreeMap<(&'static str, &'static str), Fact>,
        root_enums: BTreeMap<(&'static str, &'static str), Fact>,
        root_aliases: BTreeMap<(&'static str, &'static str), Fact>,
        module_structs: BTreeMap<(&'static str, &'static str), Fact>,
        module_enums: BTreeMap<(&'static str, &'static str), Fact>,
        module_aliases: BTreeMap<(&'static str, &'static str), Fact>,
        constructors: BTreeMap<(&'static str, &'static str), Head>,
        primitive_error: Option<SemanticProviderError<&'static str, &'static str>>,
        builtin_error: Option<SemanticProviderError<&'static str, &'static str>>,
        reduce_error: Option<SemanticProviderError<&'static str, &'static str>>,
        reduce_none: bool,
        force_value_result: bool,
        observe_error: Option<SemanticProviderError<&'static str, &'static str>>,
        array_value_error: Option<SemanticProviderError<&'static str, &'static str>>,
        array_type_error: Option<SemanticProviderError<&'static str, &'static str>>,
        allow_qualified_paths: Option<bool>,
        allow_qualified_value_heads: Option<bool>,
    }

    struct FixtureScopeRestore<'a> {
        fixture: &'a mut Fixture,
        previous_types: BTreeMap<&'static str, &'static str>,
        previous_values: BTreeMap<&'static str, i64>,
    }

    impl Drop for FixtureScopeRestore<'_> {
        fn drop(&mut self) {
            self.fixture.active_type_substitutions = std::mem::take(&mut self.previous_types);
            self.fixture.active_value_substitutions = std::mem::take(&mut self.previous_values);
        }
    }

    impl Fixture {
        fn call(&mut self, operation: &str, scope: &str, name: &str) {
            self.calls.push(format!("{operation}:{scope}:{name}"));
        }
    }

    impl SemanticModulePathProvider<&'static str, &'static str, &'static str> for Fixture {
        type Abort = &'static str;
        type Failure = &'static str;

        fn root_module_binding(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Binding>> {
            self.call("root_module", scope, name);
            Ok(self.bindings.get(&(*scope, name)).cloned())
        }

        fn module_binding(
            &mut self,
            module: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Binding>> {
            self.call("module", module, name);
            Ok(self.bindings.get(&(*module, name)).cloned())
        }

        fn module_display_name(&self, module: &&'static str) -> Arc<str> {
            Arc::from(*module)
        }

        fn accessing_domain(&self, scope: &&'static str) -> SemanticVisibilityDomain {
            self.accessing_domain_calls
                .set(self.accessing_domain_calls.get() + 1);
            SemanticVisibilityDomain::from_file_path(Some(scope))
        }
    }

    impl
        SemanticTypeSyntaxProvider<
            &'static str,
            &'static str,
            &'static str,
            &'static str,
            &'static str,
            &'static str,
            i64,
        > for Fixture
    {
        fn with_comptime_substitutions<R>(
            &mut self,
            type_substitutions: &[(&'static str, &'static str)],
            value_substitutions: &[(&'static str, i64)],
            operation: impl FnOnce(&mut Self) -> R,
        ) -> R {
            let previous_types = std::mem::replace(
                &mut self.active_type_substitutions,
                type_substitutions.iter().copied().collect(),
            );
            let previous_values = std::mem::replace(
                &mut self.active_value_substitutions,
                value_substitutions.iter().copied().collect(),
            );
            self.scope_observations
                .push((type_substitutions.to_vec(), value_substitutions.to_vec()));
            let restore = FixtureScopeRestore {
                fixture: self,
                previous_types,
                previous_values,
            };
            let result = operation(restore.fixture);
            drop(restore);
            result
        }

        fn substituted_type(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<&'static str>> {
            self.call("substitution", scope, name);
            Ok(self.active_type_substitutions.get(name).copied())
        }

        fn primitive_type(&mut self, name: &str) -> FixtureResult<Option<&'static str>> {
            self.call("primitive", "-", name);
            if let Some(error) = self.primitive_error.take() {
                return Err(error);
            }
            Ok((name == "i32").then_some("primitive:i32"))
        }

        fn builtin_type(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<&'static str>> {
            self.call("builtin", scope, name);
            Ok((name == "str").then_some("builtin:str"))
        }

        fn root_struct_type(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("root_struct", scope, name);
            Ok(self.root_structs.get(&(*scope, name)).cloned())
        }

        fn root_enum_type(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("root_enum", scope, name);
            Ok(self.root_enums.get(&(*scope, name)).cloned())
        }

        fn root_type_alias(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("root_alias", scope, name);
            Ok(self.root_aliases.get(&(*scope, name)).cloned())
        }

        fn module_struct_type(
            &mut self,
            module: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("module_struct", module, name);
            Ok(self.module_structs.get(&(*module, name)).cloned())
        }

        fn module_enum_type(
            &mut self,
            module: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("module_enum", module, name);
            Ok(self.module_enums.get(&(*module, name)).cloned())
        }

        fn module_type_alias(
            &mut self,
            module: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Fact>> {
            self.call("module_alias", module, name);
            Ok(self.module_aliases.get(&(*module, name)).cloned())
        }

        fn allows_qualified_paths(&self, _scope: &&'static str) -> bool {
            self.allow_qualified_paths.unwrap_or(true)
        }

        fn allows_qualified_comptime_call_head(
            &self,
            scope: &&'static str,
            expectation: SemanticComptimeCallExpectation,
        ) -> bool {
            if expectation == SemanticComptimeCallExpectation::Value {
                self.allow_qualified_value_heads.unwrap_or(true)
            } else {
                self.allows_qualified_paths(scope)
            }
        }

        fn resolve_array_length(
            &mut self,
            scope: &&'static str,
            length: SemanticValueSyntax<'_>,
        ) -> FixtureResult<Option<u64>> {
            self.call("array_length", scope, &format!("{length:?}"));
            match length {
                SemanticValueSyntax::Integer(value) => Ok(u64::try_from(value).ok()),
                SemanticValueSyntax::Name(name) => self
                    .active_value_substitutions
                    .get(name)
                    .copied()
                    .map(|value| u64::try_from(value).ok())
                    .ok_or(SemanticProviderError::Failure("unknown length")),
            }
        }

        fn array_length_from_value(
            &mut self,
            _scope: &&'static str,
            value: &i64,
        ) -> FixtureResult<Option<u64>> {
            if let Some(error) = self.array_value_error.take() {
                return Err(error);
            }
            Ok(u64::try_from(*value).ok())
        }

        fn array_type(
            &mut self,
            _element: &'static str,
            _length: Option<u64>,
        ) -> FixtureResult<&'static str> {
            if let Some(error) = self.array_type_error.take() {
                return Err(error);
            }
            self.calls.push("array_type".to_string());
            Ok("array")
        }

        fn ptr_const_type(&mut self, _pointee: &'static str) -> FixtureResult<&'static str> {
            self.calls.push("ptr_const".to_string());
            Ok("ptr const")
        }

        fn ptr_mut_type(&mut self, _pointee: &'static str) -> FixtureResult<&'static str> {
            self.calls.push("ptr_mut".to_string());
            Ok("ptr mut")
        }

        fn slice_type(
            &mut self,
            scope: &&'static str,
            syntax: &str,
            _element: &'static str,
        ) -> FixtureResult<&'static str> {
            self.call("slice", scope, syntax);
            Ok("slice")
        }

        fn builtin_type_call(
            &mut self,
            scope: &&'static str,
            name: &str,
            _arguments: &[SemanticValueSyntax<'_>],
        ) -> FixtureResult<Option<&'static str>> {
            self.call("builtin_call", scope, name);
            if let Some(error) = self.builtin_error.take() {
                return Err(error);
            }
            Ok(None)
        }

        fn observe_materialized_type(&mut self, _ty: &&'static str) -> FixtureResult<()> {
            if let Some(error) = self.observe_error.take() {
                return Err(error);
            }
            Ok(())
        }

        fn root_constructor(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Head>> {
            self.call("root_constructor", scope, name);
            Ok(self.constructors.get(&(*scope, name)).cloned())
        }

        fn module_constructor(
            &mut self,
            module: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<Head>> {
            self.call("module_constructor", module, name);
            Ok(self.constructors.get(&(*module, name)).cloned())
        }

        fn resolve_value_argument(
            &mut self,
            scope: &&'static str,
            constructor: &str,
            _head: &Head,
            _parameter_index: usize,
            _type_arguments: &[(&'static str, &'static str)],
            _value_arguments: &[(&'static str, i64)],
            syntax: SemanticValueSyntax<'_>,
        ) -> FixtureResult<i64> {
            self.call("value", scope, constructor);
            match syntax {
                SemanticValueSyntax::Integer(value) => i64::try_from(value)
                    .map_err(|_| SemanticProviderError::Failure("unknown value")),
                SemanticValueSyntax::Name(syntax) => syntax
                    .parse()
                    .map_err(|_| SemanticProviderError::Failure("unknown value")),
            }
        }

        fn reduce_comptime_call(
            &mut self,
            head: &Head,
            type_arguments: &[(&'static str, &'static str)],
            value_arguments: &[(&'static str, i64)],
        ) -> FixtureResult<Option<SemanticComptimeCallResult<&'static str, i64>>> {
            self.calls.push(format!("reduce:{}", head.key));
            if let Some(error) = self.reduce_error.take() {
                return Err(error);
            }
            self.reduced_arguments.extend(
                type_arguments
                    .iter()
                    .map(|(name, value)| format!("type:{name}={value}")),
            );
            self.reduced_arguments.extend(
                value_arguments
                    .iter()
                    .map(|(name, value)| format!("value:{name}={value}")),
            );
            if self.reduce_none {
                return Ok(None);
            }
            Ok(Some(if self.force_value_result {
                SemanticComptimeCallResult::Value(2)
            } else if head.returns_type {
                SemanticComptimeCallResult::Type("constructed")
            } else {
                SemanticComptimeCallResult::Value(2)
            }))
        }
    }

    fn fact(value: &'static str, site: &'static str, public: bool, file: &'static str) -> Fact {
        SemanticTypeFact {
            value,
            site,
            is_public: public,
            defining_domain: SemanticVisibilityDomain::from_file_path(Some(file)),
            defining_file: Arc::from(file),
        }
    }

    fn binding(
        target: &'static str,
        site: &'static str,
        public: bool,
        file: &'static str,
    ) -> Binding {
        SemanticModuleBinding {
            target,
            site,
            is_public: public,
            defining_domain: SemanticVisibilityDomain::from_file_path(Some(file)),
            defining_file: Arc::from(file),
        }
    }

    fn head(
        site: &'static str,
        public: bool,
        returns_type: bool,
        parameters: Vec<SemanticTypeConstructorParameter<&'static str>>,
    ) -> Head {
        SemanticTypeConstructorHead {
            key: "ctor-key",
            site,
            parameters: parameters.into(),
            returns_type,
            is_public: public,
            defining_domain: SemanticVisibilityDomain::from_file_path(Some("lib/ctor.rue")),
            defining_file: Arc::from("lib/ctor.rue"),
        }
    }

    fn type_parameter(comptime: bool) -> SemanticTypeConstructorParameter<&'static str> {
        SemanticTypeConstructorParameter {
            name: "T",
            is_comptime: comptime,
            is_type: true,
        }
    }

    fn value_parameter(comptime: bool) -> SemanticTypeConstructorParameter<&'static str> {
        SemanticTypeConstructorParameter {
            name: "N",
            is_comptime: comptime,
            is_type: false,
        }
    }

    fn structured_type(
        syntax: &str,
    ) -> (
        rue_rir::RirTypeSyntaxArena<Arc<str>>,
        rue_rir::RirTypeSyntaxRef,
    ) {
        let source = format!("fn probe(value: {syntax}) {{}}");
        let (tokens, interner): (_, ThreadedRodeo) = Lexer::new(&source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("fixture parses as a function");
        };
        let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
        let root = builder
            .push_parser_type(&function.params[0].ty, |symbol| {
                Arc::<str>::from(interner.resolve(&symbol))
            })
            .unwrap();
        (builder.finish(), root)
    }

    fn resolve_type(
        fixture: &mut Fixture,
        syntax: &str,
    ) -> Result<
        &'static str,
        SemanticTypeSyntaxError<&'static str, &'static str, &'static str, &'static str>,
    > {
        let (arena, root) = structured_type(syntax);
        resolve_structured_semantic_type_syntax(fixture, &"app/main.rue", &arena, root)
    }

    fn configure_nested_calls(fixture: &mut Fixture) {
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head("outer-site", true, true, vec![type_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "Inner"),
            head("inner-site", true, true, vec![type_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "Length"),
            head("length-site", true, false, vec![value_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "InnerValue"),
            head("inner-value-site", true, false, vec![value_parameter(true)]),
        );
    }

    #[test]
    fn structured_poll_suspends_each_call_once_without_replaying_traversal() {
        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head(
                "outer-site",
                true,
                true,
                vec![type_parameter(true), value_parameter(true)],
            ),
        );
        let (arena, root) = structured_type("Outer(Inner(i32), InnerValue(2))");
        let mut poll = poll_structured_type_machine(
            StructuredTypeMachine::new(root),
            &mut fixture,
            &&"app/main.rue",
            &arena,
            AsRef::as_ref,
        )
        .unwrap();
        let mut suspensions = 0;
        let mut request_sites: Vec<&'static str> = Vec::new();
        let result = loop {
            match poll {
                StructuredTypePoll::Ready(value) => break value,
                StructuredTypePoll::Suspended(suspension) => {
                    let suspension = *suspension;
                    suspensions += 1;
                    let request = suspension.request();
                    request_sites.push(request.head().site);
                    let reduced = fixture.reduce_comptime_call(
                        request.head(),
                        request.type_arguments(),
                        request.value_arguments(),
                    );
                    poll = suspension
                        .resume(
                            &mut fixture,
                            &&"app/main.rue",
                            &arena,
                            AsRef::as_ref,
                            reduced,
                        )
                        .unwrap();
                }
            }
        };
        assert_eq!(result, "constructed");
        assert_eq!(suspensions, 3);
        assert_eq!(
            fixture.reduced_arguments,
            [
                "type:T=primitive:i32",
                "value:N=2",
                "type:T=constructed",
                "value:N=2"
            ]
        );
        assert_eq!(
            request_sites,
            ["inner-site", "inner-value-site", "outer-site"]
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|call| call.starts_with("reduce:"))
                .count(),
            3
        );
        assert_eq!(
            fixture.calls,
            [
                "root_constructor:app/main.rue:Outer",
                "builtin_call:app/main.rue:Inner",
                "root_constructor:app/main.rue:Inner",
                "substitution:app/main.rue:i32",
                "primitive:-:i32",
                "reduce:ctor-key",
                "root_constructor:app/main.rue:InnerValue",
                "value:app/main.rue:InnerValue",
                "reduce:ctor-key",
                "reduce:ctor-key",
            ]
        );
    }

    #[test]
    fn suspended_completion_errors_keep_the_outer_type_argument_wrapper() {
        let cases = [
            ("abort", SemanticProviderError::Abort("abort")),
            ("failure", SemanticProviderError::Failure("failure")),
        ];
        for (label, error) in cases {
            let mut fixture = Fixture::default();
            configure_nested_calls(&mut fixture);
            fixture.reduce_error = Some(error);
            let error = resolve_type(&mut fixture, "Outer(Inner(i32))").unwrap_err();
            assert!(
                matches!(
                    error,
                    SemanticResolutionError::ComptimeCallTypeArgument {
                        ref constructor,
                        argument_index: 0,
                        error: ref nested,
                        ..
                    } if constructor.as_ref() == "Outer"
                        && matches!(
                            nested.as_ref(),
                            SemanticResolutionError::ProviderAbort("abort")
                                | SemanticResolutionError::ProviderFailure("failure")
                        )
                ),
                "{label}: {error:?}"
            );
        }

        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        fixture.reduce_none = true;
        let error = resolve_type(&mut fixture, "Outer(Inner(i32))").unwrap_err();
        assert!(matches!(
            error,
            SemanticResolutionError::ComptimeCallTypeArgument {
                ref constructor,
                argument_index: 0,
                error: ref nested,
                ..
            } if constructor.as_ref() == "Outer"
                && matches!(
                    nested.as_ref(),
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::ConstructorDidNotReduce { .. }
                    )
                )
        ));

        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        fixture.force_value_result = true;
        let error = resolve_type(&mut fixture, "Outer(Inner(i32))").unwrap_err();
        assert!(
            matches!(
                error,
                SemanticResolutionError::ComptimeCallTypeArgument {
                    ref constructor,
                    argument_index: 0,
                    error: ref nested,
                    ..
                } if constructor.as_ref() == "Outer"
                    && matches!(
                        nested.as_ref(),
                        SemanticResolutionError::Semantic(
                            SemanticTypeSyntaxFailure::ConstructorDidNotReduce { .. }
                        )
                    )
            ),
            "wrong kind: {error:?}"
        );

        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        let (arena, root) = structured_type("Inner(i32)");
        let poll = poll_structured_type_machine(
            StructuredTypeMachine::new(root),
            &mut fixture,
            &&"app/main.rue",
            &arena,
            AsRef::as_ref,
        )
        .unwrap();
        let StructuredTypePoll::Suspended(suspension) = poll else {
            panic!("Inner should suspend before reduction");
        };
        let suspension = *suspension;
        fixture.observe_error = Some(SemanticProviderError::Failure("observe"));
        let request = suspension.request();
        let reduced = fixture.reduce_comptime_call(
            request.head(),
            request.type_arguments(),
            request.value_arguments(),
        );
        let error = match suspension.resume(
            &mut fixture,
            &&"app/main.rue",
            &arena,
            AsRef::as_ref,
            reduced,
        ) {
            Ok(_) => panic!("observation failure should stop resume"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            SemanticResolutionError::ProviderFailure("observe")
        ));
    }

    #[test]
    fn suspended_postprocessing_errors_keep_the_outer_type_argument_wrapper() {
        for (array_value_error, array_type_error) in [
            (Some(SemanticProviderError::Failure("array value")), None),
            (None, Some(SemanticProviderError::Abort("array type"))),
        ] {
            let mut fixture = Fixture::default();
            configure_nested_calls(&mut fixture);
            fixture.array_value_error = array_value_error;
            fixture.array_type_error = array_type_error;
            let error = resolve_type(&mut fixture, "Outer([i32; Length(2)])").unwrap_err();
            assert!(matches!(
                error,
                SemanticResolutionError::ComptimeCallTypeArgument {
                    constructor,
                    argument_index: 0,
                    ..
                } if constructor.as_ref() == "Outer"
            ));
        }
    }

    #[test]
    fn fast_path_resolution_does_not_derive_the_visibility_domain() {
        // RUE-1840: the substituted, primitive and builtin fast paths resolve
        // most names and all return before any visibility check, so they must
        // not pay for the accessing domain — a path parse plus an Arc<str>
        // allocation that is then discarded. Only a selected named type reads
        // it, and then exactly once.
        let mut fixture = Fixture::default();
        assert_eq!(resolve_type(&mut fixture, "i32"), Ok("primitive:i32"));
        assert_eq!(
            fixture.accessing_domain_calls.get(),
            0,
            "a primitive resolution derived the visibility domain"
        );

        let mut fixture = Fixture::default();
        fixture.root_structs.insert(
            ("app/main.rue", "Thing"),
            fact("struct", "struct-site", true, "app/types.rue"),
        );
        assert_eq!(resolve_type(&mut fixture, "Thing"), Ok("struct"));
        assert_eq!(
            fixture.accessing_domain_calls.get(),
            1,
            "a selected named type must still derive the visibility domain"
        );
    }

    #[test]
    fn unqualified_nominal_precedes_alias_and_stops_discovery_exactly() {
        let mut fixture = Fixture::default();
        fixture.root_structs.insert(
            ("app/main.rue", "Thing"),
            fact("struct", "struct-site", true, "app/types.rue"),
        );
        fixture.root_aliases.insert(
            ("app/main.rue", "Thing"),
            fact("alias", "alias-site", true, "app/aliases.rue"),
        );

        assert_eq!(resolve_type(&mut fixture, "Thing"), Ok("struct"));
        assert_eq!(
            fixture.calls,
            [
                "substitution:app/main.rue:Thing",
                "primitive:-:Thing",
                "builtin:app/main.rue:Thing",
                "root_struct:app/main.rue:Thing",
            ]
        );
    }

    #[test]
    fn qualified_alias_visibility_and_same_directory_private_are_canonical() {
        let mut fixture = Fixture::default();
        fixture.bindings.insert(
            ("app/main.rue", "api"),
            binding("api", "api-binding", true, "app/main.rue"),
        );
        fixture.module_aliases.insert(
            ("api", "PublicAlias"),
            fact("public-alias", "public-site", true, "lib/api.rue"),
        );
        fixture.module_aliases.insert(
            ("api", "PrivateAlias"),
            fact("private-alias", "private-site", false, "lib/api.rue"),
        );
        fixture.root_structs.insert(
            ("app/main.rue", "Local"),
            fact("local", "local-site", false, "app/private.rue"),
        );

        assert_eq!(
            resolve_type(&mut fixture, "api.PublicAlias"),
            Ok("public-alias")
        );
        assert!(matches!(
            resolve_type(&mut fixture, "api.PrivateAlias"),
            Err(SemanticResolutionError::Semantic(
                SemanticTypeSyntaxFailure::PrivateItem {
                    kind: SemanticTypeFactKind::Constant,
                    site: "private-site",
                    ..
                }
            ))
        ));
        assert_eq!(resolve_type(&mut fixture, "Local"), Ok("local"));
    }

    #[test]
    fn qualified_alias_trace_has_no_constructor_or_extra_discovery() {
        let mut fixture = Fixture::default();
        fixture.bindings.insert(
            ("app/main.rue", "api"),
            binding("api", "api-binding", true, "app/main.rue"),
        );
        fixture.module_aliases.insert(
            ("api", "Alias"),
            fact("alias", "alias-site", true, "lib/api.rue"),
        );

        assert_eq!(resolve_type(&mut fixture, "api.Alias"), Ok("alias"));
        assert_eq!(
            fixture.calls,
            [
                "root_module:app/main.rue:api",
                "module_struct:api:Alias",
                "module_enum:api:Alias",
                "module_alias:api:Alias",
            ]
        );
    }

    #[test]
    fn lexical_shapes_issue_only_their_required_provider_calls() {
        let cases: &[(&str, &[&str])] = &[
            (
                "[i32; 2]",
                &[
                    "substitution:app/main.rue:i32",
                    "primitive:-:i32",
                    "array_length:app/main.rue:Integer(2)",
                    "array_type",
                ],
            ),
            (
                "ptr const i32",
                &[
                    "substitution:app/main.rue:i32",
                    "primitive:-:i32",
                    "ptr_const",
                ],
            ),
            (
                "Make(i32)",
                &[
                    "builtin_call:app/main.rue:Make",
                    "root_constructor:app/main.rue:Make",
                    "substitution:app/main.rue:i32",
                    "primitive:-:i32",
                    "reduce:ctor-key",
                ],
            ),
            (
                "[i32]",
                &[
                    "substitution:app/main.rue:i32",
                    "primitive:-:i32",
                    "slice:app/main.rue:[i32]",
                ],
            ),
        ];

        for &(syntax, expected) in cases {
            let mut fixture = Fixture::default();
            fixture.constructors.insert(
                ("app/main.rue", "Make"),
                head("make-site", true, true, vec![type_parameter(true)]),
            );
            resolve_type(&mut fixture, syntax).unwrap();
            assert_eq!(fixture.calls, expected, "unexpected trace for '{syntax}'");
        }
    }

    #[test]
    fn call_state_preserves_argument_order_and_lazy_diagnostics() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Build"),
            head(
                "build-site",
                true,
                true,
                vec![type_parameter(true), value_parameter(true)],
            ),
        );
        let mut state = SemanticComptimeCallState::admit(
            &mut fixture,
            &&"app/main.rue",
            &["Build"],
            2,
            SemanticComptimeCallExpectation::Type,
            || Arc::from("Build(...)"),
        )
        .unwrap();
        let mut rendered = false;
        state
            .accept::<&'static str, &'static str>(
                || {
                    rendered = true;
                    Arc::from("T")
                },
                Ok(ResolvedComptimeArgument::Type("i32")),
            )
            .unwrap();
        assert!(
            !rendered,
            "successful arguments must not render diagnostics"
        );
        state
            .accept::<&'static str, &'static str>(
                || Arc::from("7"),
                Ok(ResolvedComptimeArgument::Value(7)),
            )
            .unwrap();
        let request = state.into_request();
        let reduced = fixture.reduce_comptime_call(
            &request.head,
            &request.type_arguments,
            &request.value_arguments,
        );
        let resolved = request.complete(reduced).unwrap();
        assert_eq!(resolved.type_arguments, [("T", "i32")]);
        assert_eq!(resolved.value_arguments, [("N", 7)]);
        assert_eq!(fixture.reduced_arguments, ["type:T=i32", "value:N=7"]);
    }

    #[test]
    fn call_state_wraps_type_argument_errors_with_constructor_metadata() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Build"),
            head("build-site", true, true, vec![type_parameter(true)]),
        );
        let mut state = SemanticComptimeCallState::admit(
            &mut fixture,
            &&"app/main.rue",
            &["Build"],
            1,
            SemanticComptimeCallExpectation::Type,
            || Arc::from("Build(...)"),
        )
        .unwrap();
        let nested = SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::UnknownType {
            syntax: Arc::from("Missing"),
        });
        let error = state
            .accept::<&'static str, &'static str>(|| Arc::from("Missing"), Err(nested))
            .unwrap_err();
        match error {
            SemanticResolutionError::ComptimeCallTypeArgument {
                constructor,
                argument_index,
                argument,
                error,
            } => {
                assert_eq!(constructor.as_ref(), "Build");
                assert_eq!(argument_index, 0);
                assert_eq!(argument.as_ref(), "Missing");
                assert!(matches!(
                    *error,
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::UnknownType { .. }
                    )
                ));
            }
            other => panic!("unexpected wrapped error: {other:?}"),
        }
    }

    #[test]
    fn structured_shapes_share_one_resolution_policy() {
        for syntax in [
            "api.Alias",
            "[i32; 2]",
            "ptr const i32",
            "ptr mut ptr const i32",
            "Make(i32)",
            "Buffer(2)",
            "[i32]",
        ] {
            let configured = || {
                let mut fixture = Fixture::default();
                fixture.bindings.insert(
                    ("app/main.rue", "api"),
                    binding("api", "api-binding", true, "app/main.rue"),
                );
                fixture.module_aliases.insert(
                    ("api", "Alias"),
                    fact("alias", "alias-site", true, "lib/api.rue"),
                );
                fixture.constructors.insert(
                    ("app/main.rue", "Make"),
                    head("make-site", true, true, vec![type_parameter(true)]),
                );
                fixture.constructors.insert(
                    ("app/main.rue", "Buffer"),
                    head("buffer-site", true, true, vec![value_parameter(true)]),
                );
                fixture
            };
            let (arena, root) = structured_type(syntax);
            let mut structured = configured();
            let structured_result = resolve_structured_semantic_type_syntax(
                &mut structured,
                &"app/main.rue",
                &arena,
                root,
            )
            .unwrap();

            assert!(!structured_result.is_empty(), "result for `{syntax}`");
            assert!(!structured.calls.is_empty(), "work trace for `{syntax}`");
        }
    }

    #[test]
    fn structured_calls_keep_nested_type_and_value_arguments_in_order() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head(
                "outer-site",
                true,
                true,
                vec![type_parameter(true), value_parameter(true)],
            ),
        );
        fixture.constructors.insert(
            ("app/main.rue", "InnerType"),
            head("inner-type-site", true, true, vec![type_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "InnerValue"),
            head("inner-value-site", true, false, vec![value_parameter(true)]),
        );

        assert_eq!(
            resolve_type(&mut fixture, "Outer(InnerType(i32), InnerValue(2))"),
            Ok("constructed")
        );
        assert_eq!(
            fixture.reduced_arguments,
            [
                "type:T=primitive:i32",
                "value:N=2",
                "type:T=constructed",
                "value:N=2",
            ],
            "nested reductions retain earlier binding prefixes and source order"
        );
    }

    #[test]
    fn structured_type_argument_errors_keep_the_outer_wrapper() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head("outer-site", true, true, vec![type_parameter(true)]),
        );
        let error = resolve_type(&mut fixture, "Outer(ptr const Missing)").unwrap_err();
        assert!(matches!(
            error,
            SemanticResolutionError::ComptimeCallTypeArgument {
                constructor,
                argument_index: 0,
                ..
            } if constructor.as_ref() == "Outer"
        ));
    }

    #[test]
    fn nested_call_failures_keep_every_type_argument_wrapper() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head("outer-site", true, true, vec![type_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "Inner"),
            head("inner-site", true, true, vec![type_parameter(true)]),
        );

        let error = resolve_type(&mut fixture, "Outer(Inner(Missing))").unwrap_err();
        let SemanticResolutionError::ComptimeCallTypeArgument {
            constructor,
            argument_index: 0,
            error,
            ..
        } = error
        else {
            panic!("outer call must wrap its failing type argument")
        };
        assert_eq!(constructor.as_ref(), "Outer");
        let SemanticResolutionError::ComptimeCallTypeArgument {
            constructor,
            argument_index: 0,
            error,
            ..
        } = *error
        else {
            panic!("inner call must retain its own type-argument wrapper")
        };
        assert_eq!(constructor.as_ref(), "Inner");
        assert!(matches!(
            *error,
            SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::UnknownType { .. })
        ));
    }

    #[test]
    fn value_call_is_not_reinterpreted_as_a_type_argument() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head("outer-site", true, true, vec![type_parameter(true)]),
        );
        fixture.constructors.insert(
            ("app/main.rue", "ValueMaker"),
            head("value-site", true, false, vec![value_parameter(true)]),
        );
        let error = resolve_type(&mut fixture, "Outer(ValueMaker(2))").unwrap_err();
        assert!(matches!(
            error,
            SemanticResolutionError::ComptimeCallTypeArgument { .. }
        ));
    }

    #[test]
    fn builtin_type_argument_failures_keep_the_outer_wrapper() {
        let mut fixture = Fixture {
            builtin_error: Some(SemanticProviderError::Failure("builtin failed")),
            ..Fixture::default()
        };
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head("outer-site", true, true, vec![type_parameter(true)]),
        );
        let error = resolve_type(&mut fixture, "Outer(Builtin(i32))").unwrap_err();
        assert!(matches!(
            error,
            SemanticResolutionError::ComptimeCallTypeArgument {
                constructor,
                argument_index: 0,
                error,
                ..
            } if constructor.as_ref() == "Outer"
                && matches!(*error, SemanticResolutionError::ProviderFailure("builtin failed"))
        ));
    }

    #[test]
    fn structured_failures_preserve_diagnostics_and_bounded_work() {
        for syntax in [
            "Missing",
            "api.PrivateAlias",
            "ValueMaker(i32)",
            "Make(i32, i32)",
            "Make(2)",
        ] {
            let configured = || {
                let mut fixture = Fixture::default();
                fixture.bindings.insert(
                    ("app/main.rue", "api"),
                    binding("api", "api-binding", true, "app/main.rue"),
                );
                fixture.module_aliases.insert(
                    ("api", "PrivateAlias"),
                    fact("private", "private-site", false, "lib/api.rue"),
                );
                fixture.constructors.insert(
                    ("app/main.rue", "Make"),
                    head("make-site", true, true, vec![type_parameter(true)]),
                );
                fixture.constructors.insert(
                    ("app/main.rue", "ValueMaker"),
                    head("value-maker-site", true, false, vec![type_parameter(true)]),
                );
                fixture
            };

            let (arena, root) = structured_type(syntax);
            let mut structured = configured();
            let structured_result = resolve_structured_semantic_type_syntax(
                &mut structured,
                &"app/main.rue",
                &arena,
                root,
            );

            assert!(
                structured_result.is_err(),
                "fixture must fail for `{syntax}`"
            );
            assert!(!structured.calls.is_empty(), "work trace for `{syntax}`");
        }
    }

    #[test]
    fn nested_shape_failures_report_the_failing_child_syntax() {
        for syntax in ["[Missing; 2]", "ptr const Missing"] {
            let (arena, root) = structured_type(syntax);
            let mut fixture = Fixture::default();
            let error = resolve_structured_semantic_type_syntax(
                &mut fixture,
                &"app/main.rue",
                &arena,
                root,
            )
            .unwrap_err();
            match error {
                SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::UnknownType {
                    syntax,
                }) => assert_eq!(syntax.as_ref(), "Missing"),
                other => panic!("unexpected nested diagnostic: {other:?}"),
            }
        }
    }

    #[test]
    fn nested_array_failures_report_the_failing_array_syntax() {
        let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
        let element = builder.push_named_type(Arc::<str>::from("i32")).unwrap();
        let invalid_length = builder.push_unit_type().unwrap();
        let inner = builder.push_array_type(element, invalid_length).unwrap();
        let outer_length = builder.push_integer(2).unwrap();
        let root = builder.push_array_type(inner, outer_length).unwrap();
        let arena = builder.finish();
        let mut fixture = Fixture::default();
        let error =
            resolve_structured_semantic_type_syntax(&mut fixture, &"app/main.rue", &arena, root)
                .unwrap_err();
        match error {
            SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::UnknownType {
                syntax,
            }) => {
                assert_eq!(syntax.as_ref(), "[i32; ()]");
            }
            other => panic!("unexpected nested array diagnostic: {other:?}"),
        }
    }

    #[test]
    fn structured_array_length_call_never_renders_and_reparses_the_call() {
        let (arena, root) = structured_type("[i32; Width(2)]");
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Width"),
            head("width-site", true, false, vec![value_parameter(true)]),
        );

        assert_eq!(
            resolve_structured_semantic_type_syntax(&mut fixture, &"app/main.rue", &arena, root,),
            Ok("array")
        );
        assert_eq!(
            fixture.calls,
            [
                "substitution:app/main.rue:i32",
                "primitive:-:i32",
                "root_constructor:app/main.rue:Width",
                "value:app/main.rue:Width",
                "reduce:ctor-key",
                "array_type",
            ]
        );
    }

    #[test]
    fn keyed_structured_job_owns_program_and_resumes_without_authority_arguments() {
        let (arena, root) = structured_type("Outer(i32)");
        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        let authority = ComptimeStructuredTypeAuthority::from_registered(
            "program-a",
            "app/main.rue",
            arena.clone(),
            Arc::from(arena.symbols()),
            root,
        );
        let poll = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture,
            authority,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let ComptimeStructuredTypePoll::Suspended(job) = poll else {
            panic!("constructor call must suspend before reduction");
        };
        assert_eq!(job.program(), &"program-a");
        assert_eq!(job.type_arguments(), &[("T", "primitive:i32")]);
        let request = job.request_view();
        assert_eq!(request.program(), &"program-a");
        assert_eq!(request.type_arguments(), &[("T", "primitive:i32")]);
        let reduced = fixture.reduce_comptime_call(
            request.head(),
            request.type_arguments(),
            request.value_arguments(),
        );
        let next = job
            .resume::<&'static str, _>(&mut fixture, reduced)
            .unwrap();
        assert!(matches!(
            next,
            ComptimeStructuredTypePoll::Ready("constructed")
        ));

        let mut fixture_b = Fixture::default();
        configure_nested_calls(&mut fixture_b);
        let authority_b = ComptimeStructuredTypeAuthority::from_registered(
            "program-b",
            "app/main.rue",
            arena,
            Arc::from([Arc::<str>::from("Outer"), Arc::<str>::from("i32")]),
            root,
        );
        let poll_b = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture_b,
            authority_b,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let ComptimeStructuredTypePoll::Suspended(job_b) = poll_b else {
            panic!("colliding program must suspend before reduction");
        };
        assert_eq!(job_b.program(), &"program-b");
        assert_eq!(job_b.request_view().program(), &"program-b");
    }

    #[test]
    fn structured_jobs_restore_and_reinstall_their_owned_scopes_when_interleaved() {
        let (arena, root) = structured_type("Outer(Inner(i32), T, [i32; N])");
        let mut fixture = Fixture::default();
        configure_nested_calls(&mut fixture);
        fixture.constructors.insert(
            ("app/main.rue", "Outer"),
            head(
                "outer-site",
                true,
                true,
                vec![
                    SemanticTypeConstructorParameter {
                        name: "InnerT",
                        is_comptime: true,
                        is_type: true,
                    },
                    SemanticTypeConstructorParameter {
                        name: "ScopedT",
                        is_comptime: true,
                        is_type: true,
                    },
                    SemanticTypeConstructorParameter {
                        name: "ArrayT",
                        is_comptime: true,
                        is_type: true,
                    },
                ],
            ),
        );
        let poll_a = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture,
            ComptimeStructuredTypeAuthority::from_registered(
                "program-a",
                "app/main.rue",
                arena.clone(),
                Arc::from(arena.symbols()),
                root,
            ),
            vec![("T", "scope-a")],
            vec![("N", 11)],
        )
        .unwrap();
        let ComptimeStructuredTypePoll::Suspended(job_a) = poll_a else {
            panic!("first constructor call must suspend");
        };
        let poll_b = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture,
            ComptimeStructuredTypeAuthority::from_registered(
                "program-b",
                "app/main.rue",
                arena.clone(),
                Arc::from(arena.symbols()),
                root,
            ),
            vec![("T", "scope-b")],
            vec![("N", 22)],
        )
        .unwrap();
        let ComptimeStructuredTypePoll::Suspended(job_b) = poll_b else {
            panic!("second constructor call must suspend");
        };

        // A caller may have unrelated ambient state while a continuation is
        // polled; the job's exact scope temporarily wins and is then restored.
        fixture
            .active_type_substitutions
            .insert("ambient", "ambient-type");
        fixture.active_value_substitutions.insert("ambient", 99);
        let next_a = job_a
            .resume::<&'static str, _>(
                &mut fixture,
                Ok(Some(SemanticComptimeCallResult::Type("inner-a"))),
            )
            .unwrap();
        let ComptimeStructuredTypePoll::Suspended(outer_a) = next_a else {
            panic!("first outer call must suspend after consuming its captured scope");
        };
        assert_eq!(
            outer_a.type_arguments(),
            &[
                ("InnerT", "inner-a"),
                ("ScopedT", "scope-a"),
                ("ArrayT", "array"),
            ]
        );
        assert_eq!(
            fixture.active_type_substitutions.get("ambient"),
            Some(&"ambient-type")
        );
        assert_eq!(fixture.active_value_substitutions.get("ambient"), Some(&99));

        let next_b = job_b
            .resume::<&'static str, _>(
                &mut fixture,
                Ok(Some(SemanticComptimeCallResult::Type("inner-b"))),
            )
            .unwrap();
        let ComptimeStructuredTypePoll::Suspended(outer_b) = next_b else {
            panic!("second outer call must suspend after consuming its captured scope");
        };
        assert_eq!(
            outer_b.type_arguments(),
            &[
                ("InnerT", "inner-b"),
                ("ScopedT", "scope-b"),
                ("ArrayT", "array"),
            ]
        );

        let ready_a = outer_a
            .resume::<&'static str, _>(
                &mut fixture,
                Ok(Some(SemanticComptimeCallResult::Type("constructed"))),
            )
            .unwrap();
        assert!(matches!(
            ready_a,
            ComptimeStructuredTypePoll::Ready("constructed")
        ));
        let ready_b = outer_b
            .resume::<&'static str, _>(
                &mut fixture,
                Ok(Some(SemanticComptimeCallResult::Type("constructed"))),
            )
            .unwrap();
        assert!(matches!(
            ready_b,
            ComptimeStructuredTypePoll::Ready("constructed")
        ));
        assert_eq!(
            fixture.scope_observations,
            [
                (vec![("T", "scope-a")], vec![("N", 11)]),
                (vec![("T", "scope-b")], vec![("N", 22)]),
                (vec![("T", "scope-a")], vec![("N", 11)]),
                (vec![("T", "scope-b")], vec![("N", 22)]),
                (vec![("T", "scope-a")], vec![("N", 11)]),
                (vec![("T", "scope-b")], vec![("N", 22)]),
            ]
        );
        assert_eq!(
            fixture.active_type_substitutions,
            BTreeMap::from([("ambient", "ambient-type")])
        );
        assert_eq!(
            fixture.active_value_substitutions,
            BTreeMap::from([("ambient", 99)])
        );
    }

    #[test]
    fn structured_scope_restores_ambient_maps_after_failure_and_abort() {
        let (arena, root) = structured_type("[i32; N]");
        let mut fixture = Fixture {
            active_type_substitutions: BTreeMap::from([("ambient", "old-type")]),
            active_value_substitutions: BTreeMap::from([("ambient", 23)]),
            array_type_error: Some(SemanticProviderError::Failure("array failure")),
            ..Fixture::default()
        };
        let failed = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture,
            ComptimeStructuredTypeAuthority::from_registered(
                "failure-program",
                "app/main.rue",
                arena.clone(),
                Arc::from(arena.symbols()),
                root,
            ),
            vec![("T", "transient-type")],
            vec![("N", 7)],
        );
        assert!(matches!(
            failed,
            Err(SemanticTypeSyntaxError::ProviderFailure("array failure"))
        ));
        assert_eq!(
            fixture.active_type_substitutions,
            BTreeMap::from([("ambient", "old-type")])
        );
        assert_eq!(
            fixture.active_value_substitutions,
            BTreeMap::from([("ambient", 23)])
        );

        fixture.array_type_error = Some(SemanticProviderError::Abort("cancelled"));
        let aborted = ComptimeStructuredTypeJob::begin::<&'static str, _>(
            &mut fixture,
            ComptimeStructuredTypeAuthority::from_registered(
                "abort-program",
                "app/main.rue",
                arena.clone(),
                Arc::from(arena.symbols()),
                root,
            ),
            vec![("T", "transient-type")],
            vec![("N", 8)],
        );
        assert!(matches!(
            aborted,
            Err(SemanticTypeSyntaxError::ProviderAbort("cancelled"))
        ));
        assert_eq!(
            fixture.active_type_substitutions,
            BTreeMap::from([("ambient", "old-type")])
        );
        assert_eq!(
            fixture.active_value_substitutions,
            BTreeMap::from([("ambient", 23)])
        );
    }

    #[test]
    #[should_panic(expected = "scoped poll panic")]
    fn structured_scope_restores_ambient_maps_after_panic() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct RestorationAssertion {
            fixture: Rc<RefCell<Fixture>>,
        }

        impl Drop for RestorationAssertion {
            fn drop(&mut self) {
                let fixture = self.fixture.borrow();
                assert_eq!(
                    fixture.active_type_substitutions,
                    BTreeMap::from([("ambient", "old-type")])
                );
                assert_eq!(
                    fixture.active_value_substitutions,
                    BTreeMap::from([("ambient", 23)])
                );
            }
        }

        let fixture = Rc::new(RefCell::new(Fixture {
            active_type_substitutions: BTreeMap::from([("ambient", "old-type")]),
            active_value_substitutions: BTreeMap::from([("ambient", 23)]),
            ..Fixture::default()
        }));
        let _assertion = RestorationAssertion {
            fixture: Rc::clone(&fixture),
        };
        fixture.borrow_mut().with_comptime_substitutions(
            &[("T", "panic-type")],
            &[("N", 9)],
            |_| -> () { panic!("scoped poll panic") },
        );
    }

    #[test]
    fn value_call_type_argument_uses_the_canonical_type_policy() {
        let mut fixture = Fixture::default();
        fixture.constructors.insert(
            ("app/main.rue", "Width"),
            head("width-site", true, false, vec![type_parameter(true)]),
        );
        fixture.root_structs.insert(
            ("app/main.rue", "Leaf"),
            fact("leaf", "leaf-site", true, "lib/types.rue"),
        );

        assert_eq!(
            resolve_type(&mut fixture, "[i32; Width(Leaf)]"),
            Ok("array"),
            "a value call's type argument uses the same nominal lookup policy"
        );
    }

    #[test]
    fn disallowed_qualified_paths_issue_no_fact_queries() {
        for syntax in ["api.Type", "api.Make(i32)"] {
            let mut fixture = Fixture {
                allow_qualified_paths: Some(false),
                ..Fixture::default()
            };
            assert!(resolve_type(&mut fixture, syntax).is_err());
            assert!(fixture.calls.is_empty(), "unexpected trace for '{syntax}'");
        }
    }

    #[derive(Clone, Copy)]
    enum FailureCase {
        UnknownRoot,
        UnknownIntermediate,
        NonTypeConstructor,
        Arity,
        RuntimeParameter,
        ValueWhereTypeExpected,
        PrivateConstructor,
    }

    #[test]
    fn table_driven_path_and_constructor_failures_retain_selected_sites() {
        let cases = [
            FailureCase::UnknownRoot,
            FailureCase::UnknownIntermediate,
            FailureCase::NonTypeConstructor,
            FailureCase::Arity,
            FailureCase::RuntimeParameter,
            FailureCase::ValueWhereTypeExpected,
            FailureCase::PrivateConstructor,
        ];

        for case in cases {
            let mut fixture = Fixture::default();
            let (syntax, expected_site) = match case {
                FailureCase::UnknownRoot => ("missing.Type", None),
                FailureCase::UnknownIntermediate => {
                    fixture.bindings.insert(
                        ("app/main.rue", "api"),
                        binding("api", "api-site", true, "app/main.rue"),
                    );
                    ("api.missing.Type", Some("api-site"))
                }
                FailureCase::NonTypeConstructor => {
                    fixture.constructors.insert(
                        ("app/main.rue", "Make"),
                        head("non-type-site", true, false, Vec::new()),
                    );
                    ("Make()", Some("non-type-site"))
                }
                FailureCase::Arity => {
                    fixture.constructors.insert(
                        ("app/main.rue", "Make"),
                        head("arity-site", true, true, vec![type_parameter(true)]),
                    );
                    ("Make()", Some("arity-site"))
                }
                FailureCase::RuntimeParameter => {
                    fixture.constructors.insert(
                        ("app/main.rue", "Make"),
                        head("runtime-site", true, true, vec![type_parameter(false)]),
                    );
                    ("Make(i32)", Some("runtime-site"))
                }
                FailureCase::ValueWhereTypeExpected => {
                    fixture.constructors.insert(
                        ("app/main.rue", "Make"),
                        head("kind-site", true, true, vec![type_parameter(true)]),
                    );
                    ("Make(2)", Some("kind-site"))
                }
                FailureCase::PrivateConstructor => {
                    fixture.constructors.insert(
                        ("app/main.rue", "Make"),
                        head("private-ctor-site", false, true, Vec::new()),
                    );
                    ("Make()", Some("private-ctor-site"))
                }
            };

            let error =
                resolve_type(&mut fixture, syntax).expect_err("case must fail semantically");
            match (case, error) {
                (
                    FailureCase::UnknownRoot,
                    SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::Path(
                        SemanticModulePathFailure::UnknownRoot { name },
                    )),
                ) => assert_eq!(&*name, "missing"),
                (
                    FailureCase::UnknownIntermediate,
                    SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::Path(
                        SemanticModulePathFailure::UnknownMember { module_site, .. },
                    )),
                ) => assert_eq!(Some(module_site), expected_site),
                (
                    FailureCase::NonTypeConstructor,
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::NotTypeConstructor { site, .. },
                    ),
                )
                | (
                    FailureCase::Arity,
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::InvalidConstructorArity { site, .. },
                    ),
                )
                | (
                    FailureCase::RuntimeParameter,
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::RuntimeConstructorParameter { site, .. },
                    ),
                )
                | (
                    FailureCase::ValueWhereTypeExpected,
                    SemanticResolutionError::Semantic(
                        SemanticTypeSyntaxFailure::ValueWhereTypeExpected { site, .. },
                    ),
                )
                | (
                    FailureCase::PrivateConstructor,
                    SemanticResolutionError::Semantic(SemanticTypeSyntaxFailure::PrivateItem {
                        site,
                        ..
                    }),
                ) => assert_eq!(Some(site), expected_site),
                _ => panic!("unexpected failure shape for table case"),
            }
        }
    }

    #[test]
    fn provider_failure_and_abort_remain_distinct_and_stop_policy_immediately() {
        for (provider_error, expected) in [
            (
                SemanticProviderError::Failure("bad source"),
                SemanticResolutionError::ProviderFailure("bad source"),
            ),
            (
                SemanticProviderError::Abort("cancelled"),
                SemanticResolutionError::ProviderAbort("cancelled"),
            ),
        ] {
            let mut fixture = Fixture {
                primitive_error: Some(provider_error),
                ..Fixture::default()
            };
            assert_eq!(resolve_type(&mut fixture, "i32"), Err(expected));
            assert_eq!(
                fixture.calls,
                ["substitution:app/main.rue:i32", "primitive:-:i32"]
            );
        }
    }
}
