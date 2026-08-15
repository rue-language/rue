//! Canonical, value-only type-syntax resolution policy.
//!
//! Providers expose orthogonal declaration facts and materialization hooks.
//! This module alone owns syntax routing, namespace precedence, recursive
//! structural resolution, qualified-path walking, visibility, and constructor
//! argument binding.

use std::path::Path;
use std::sync::Arc;

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
///
/// `Rendered` belongs only to the temporary adapter for legacy RIR type
/// spellings. Parser-owned declaration artifacts produce `Integer` or `Name`,
/// so their semantic path never tokenizes or reparses text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticValueSyntax<'a> {
    Integer(i128),
    Name(&'a str),
    Rendered(&'a str),
}

impl<'a> SemanticValueSyntax<'a> {
    pub fn from_rendered(syntax: &'a str) -> Self {
        let syntax = syntax.trim();
        if let Ok(value) = syntax.parse::<i128>() {
            Self::Integer(value)
        } else if is_unqualified_name_syntax(syntax) {
            Self::Name(syntax)
        } else {
            Self::Rendered(syntax)
        }
    }
}

pub trait SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>:
    SemanticModulePathProvider<S, M, A>
{
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

    fn preflight_slice(
        &mut self,
        scope: &S,
        syntax: &str,
    ) -> SemanticProviderResult<(), Self::Abort, Self::Failure>;

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
    },
    RuntimeConstructorParameter {
        constructor: Arc<str>,
        site: A,
        expected: usize,
        found: usize,
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

fn is_slice_syntax(syntax: &str) -> Option<&str> {
    (syntax.starts_with('[')
        && syntax.ends_with(']')
        && crate::parse_array_type_syntax(syntax).is_none())
    .then(|| syntax[1..syntax.len() - 1].trim())
}

fn is_unqualified_name_syntax(syntax: &str) -> bool {
    if matches!(syntax, "()" | "!") {
        return true;
    }
    let mut characters = syntax.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
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
    let accessing = provider.accessing_domain(root_scope);
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
            &accessing,
        )
        .map(Some);
    }
    if let Some(fact) = lift_provider(provider.root_enum_type(root_scope, name))? {
        return select_named_type(provider, fact, SemanticTypeFactKind::Enum, name, &accessing)
            .map(Some);
    }
    if let Some(fact) = lift_provider(provider.root_type_alias(root_scope, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Constant,
            name,
            &accessing,
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
    let accessing = provider.accessing_domain(root_scope);
    let resolved = resolve_semantic_module_path(provider, root_scope, prefix)
        .map_err(|error| error.map_semantic(F::Path))?;
    if let Some(fact) = lift_provider(provider.module_struct_type(&resolved.module, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Struct,
            name,
            &accessing,
        );
    }
    if let Some(fact) = lift_provider(provider.module_enum_type(&resolved.module, name))? {
        return select_named_type(provider, fact, SemanticTypeFactKind::Enum, name, &accessing);
    }
    if let Some(fact) = lift_provider(provider.module_type_alias(&resolved.module, name))? {
        return select_named_type(
            provider,
            fact,
            SemanticTypeFactKind::Constant,
            name,
            &accessing,
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

fn resolve_semantic_comptime_call_core<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    call_segments: &[&str],
    argument_count: usize,
    expectation: SemanticComptimeCallExpectation,
    mut call_display: impl FnMut() -> Arc<str>,
    mut argument_display: impl FnMut(usize) -> Arc<str>,
    mut argument_is_literal_value: impl FnMut(usize) -> bool,
    mut resolve_argument: impl FnMut(
        &mut P,
        usize,
        bool,
        &SemanticTypeConstructorHead<K, N, A>,
        &[(N, T)],
        &[(N, V)],
    ) -> Result<
        ResolvedComptimeArgument<T, V>,
        SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
    >,
) -> Result<
    SemanticResolvedComptimeCall<K, N, A, T, V>,
    SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
>
where
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    use SemanticResolutionError as E;
    use SemanticTypeSyntaxFailure as F;

    let accessing = provider.accessing_domain(root_scope);
    if call_segments.is_empty()
        || call_segments.iter().any(|segment| segment.is_empty())
        || (call_segments.len() > 1
            && !provider.allows_qualified_comptime_call_head(root_scope, expectation))
    {
        return Err(E::Semantic(F::UnknownType {
            syntax: call_display(),
        }));
    }
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
        E::Semantic(F::UnknownType {
            syntax: call_display(),
        })
    })?;
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
            constructor: Arc::from(call_segments.join(".")),
            site: head.site,
        }));
    }
    if expectation == SemanticComptimeCallExpectation::Value && head.returns_type {
        return Err(E::Semantic(F::TypeWhereValueExpected {
            constructor: Arc::from(call_segments.join(".")),
            site: head.site,
        }));
    }
    if head.parameters.len() != argument_count {
        return Err(E::Semantic(F::InvalidConstructorArity {
            constructor: Arc::from(call_segments.join(".")),
            site: head.site,
            expected: head.parameters.len(),
            found: argument_count,
        }));
    }
    let eligible = head
        .parameters
        .iter()
        .all(|parameter| parameter.is_comptime)
        && (head.returns_type || !head.parameters.is_empty());
    if !eligible {
        return Err(E::Semantic(F::RuntimeConstructorParameter {
            constructor: Arc::from(call_segments.join(".")),
            site: head.site,
            expected: head.parameters.len(),
            found: argument_count,
        }));
    }

    let constructor_site = head.site.clone();
    let mut type_arguments = Vec::new();
    let mut value_arguments = Vec::new();
    for parameter_index in 0..argument_count {
        let parameter = &head.parameters[parameter_index];
        if parameter.is_type && argument_is_literal_value(parameter_index) {
            return Err(E::Semantic(F::ValueWhereTypeExpected {
                constructor: Arc::from(call_segments.join(".")),
                site: constructor_site.clone(),
                argument: argument_display(parameter_index),
                parameter: parameter.name.clone(),
            }));
        }
        match resolve_argument(
            provider,
            parameter_index,
            parameter.is_type,
            &head,
            &type_arguments,
            &value_arguments,
        ) {
            Ok(ResolvedComptimeArgument::Type(value)) if parameter.is_type => {
                type_arguments.push((parameter.name.clone(), value));
            }
            Ok(ResolvedComptimeArgument::Value(value)) if !parameter.is_type => {
                value_arguments.push((parameter.name.clone(), value));
            }
            Ok(ResolvedComptimeArgument::Value(_)) => {
                return Err(E::Semantic(F::ValueWhereTypeExpected {
                    constructor: Arc::from(call_segments.join(".")),
                    site: constructor_site.clone(),
                    argument: argument_display(parameter_index),
                    parameter: parameter.name.clone(),
                }));
            }
            Ok(ResolvedComptimeArgument::Type(_)) => {
                return Err(E::Semantic(F::TypeWhereValueExpected {
                    constructor: Arc::from(call_segments.join(".")),
                    site: constructor_site.clone(),
                }));
            }
            Err(error) if parameter.is_type => {
                return Err(E::ComptimeCallTypeArgument {
                    constructor: Arc::from(call_segments.join(".")),
                    argument_index: parameter_index,
                    argument: argument_display(parameter_index),
                    error: Box::new(error),
                });
            }
            Err(error) => return Err(error),
        }
    }
    let result =
        lift_provider(provider.reduce_comptime_call(&head, &type_arguments, &value_arguments))?
            .ok_or_else(|| {
                E::Semantic(F::ConstructorDidNotReduce {
                    constructor: Arc::from(call_segments.join(".")),
                    site: constructor_site,
                })
            })?;
    if expectation == SemanticComptimeCallExpectation::Type
        && !matches!(result, SemanticComptimeCallResult::Type(_))
    {
        return Err(E::Semantic(F::ConstructorDidNotReduce {
            constructor: Arc::from(call_segments.join(".")),
            site: head.site,
        }));
    }
    Ok(SemanticResolvedComptimeCall {
        head,
        type_arguments,
        value_arguments,
        result,
    })
}

pub fn resolve_semantic_comptime_call<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    call_path: &str,
    arguments: &[String],
    expectation: SemanticComptimeCallExpectation,
) -> Result<
    SemanticResolvedComptimeCall<K, N, A, T, V>,
    SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
>
where
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    let segments = call_path.split('.').collect::<Vec<_>>();
    resolve_semantic_comptime_call_core(
        provider,
        root_scope,
        &segments,
        arguments.len(),
        expectation,
        || Arc::from(format!("{}({})", call_path, arguments.join(", "))),
        |index| Arc::from(arguments[index].as_str()),
        |index| arguments[index].trim().parse::<i128>().is_ok(),
        |provider, parameter_index, is_type, head, type_arguments, value_arguments| {
            let argument = &arguments[parameter_index];
            if is_type {
                resolve_semantic_type_syntax(provider, root_scope, argument)
                    .map(ResolvedComptimeArgument::Type)
            } else {
                lift_provider(provider.resolve_value_argument(
                    root_scope,
                    call_path,
                    head,
                    parameter_index,
                    type_arguments,
                    value_arguments,
                    SemanticValueSyntax::from_rendered(argument),
                ))
                .map(ResolvedComptimeArgument::Value)
            }
        },
    )
}

fn structured_path<Sym: AsRef<str>>(
    arena: &rue_rir::RirTypeSyntaxArena<Sym>,
    range: rue_rir::RirTypeSyntaxRange,
) -> Option<Vec<&str>> {
    arena
        .words(range)?
        .iter()
        .map(|word| {
            arena
                .symbol(rue_rir::RirTypeSyntaxSymbol::from_u32(*word))
                .map(AsRef::as_ref)
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

fn structured_syntax_display<Sym: AsRef<str>>(
    arena: &rue_rir::RirTypeSyntaxArena<Sym>,
    reference: rue_rir::RirTypeSyntaxRef,
) -> Arc<str> {
    arena
        .render_type(reference)
        .map(Arc::from)
        .unwrap_or_else(|| Arc::from("<invalid structured type syntax>"))
}

fn structured_value_syntax<'a, Sym: AsRef<str>>(
    arena: &'a rue_rir::RirTypeSyntaxArena<Sym>,
    reference: rue_rir::RirTypeSyntaxRef,
) -> Option<SemanticValueSyntax<'a>> {
    match arena.node(reference)? {
        rue_rir::RirTypeSyntaxNode::Integer(value) => Some(SemanticValueSyntax::Integer(*value)),
        rue_rir::RirTypeSyntaxNode::Named(symbol) => arena
            .symbol(*symbol)
            .map(|symbol| SemanticValueSyntax::Name(symbol.as_ref())),
        _ => None,
    }
}

fn resolve_structured_semantic_comptime_call<S, Sym, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    arena: &rue_rir::RirTypeSyntaxArena<Sym>,
    call_reference: rue_rir::RirTypeSyntaxRef,
    call_segments: Vec<&str>,
    arguments: Vec<rue_rir::RirTypeSyntaxRef>,
    expectation: SemanticComptimeCallExpectation,
) -> Result<
    SemanticResolvedComptimeCall<K, N, A, T, V>,
    SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>,
>
where
    Sym: AsRef<str>,
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    let constructor = call_segments.join(".");
    resolve_semantic_comptime_call_core(
        provider,
        root_scope,
        &call_segments,
        arguments.len(),
        expectation,
        || structured_syntax_display(arena, call_reference),
        |index| structured_syntax_display(arena, arguments[index]),
        |index| {
            matches!(
                arena.node(arguments[index]),
                Some(rue_rir::RirTypeSyntaxNode::Integer(_))
            )
        },
        |provider, parameter_index, is_type, head, type_arguments, value_arguments| {
            let argument = arguments[parameter_index];
            if is_type {
                return resolve_structured_semantic_type_syntax(
                    provider, root_scope, arena, argument,
                )
                .map(ResolvedComptimeArgument::Type);
            }
            if let Some(syntax) = structured_value_syntax(arena, argument) {
                return lift_provider(provider.resolve_value_argument(
                    root_scope,
                    &constructor,
                    head,
                    parameter_index,
                    type_arguments,
                    value_arguments,
                    syntax,
                ))
                .map(ResolvedComptimeArgument::Value);
            }
            let call = match arena.node(argument) {
                Some(rue_rir::RirTypeSyntaxNode::TypeCall { path, arguments }) => Some((
                    structured_path(arena, *path),
                    structured_references(arena, *arguments),
                )),
                Some(rue_rir::RirTypeSyntaxNode::ValueCall { name, arguments }) => Some((
                    arena.symbol(*name).map(|name| vec![name.as_ref()]),
                    structured_references(arena, *arguments),
                )),
                _ => None,
            };
            if let Some((Some(path), Some(arguments))) = call {
                let resolved = resolve_structured_semantic_comptime_call(
                    provider,
                    root_scope,
                    arena,
                    argument,
                    path,
                    arguments,
                    SemanticComptimeCallExpectation::Value,
                )?;
                return match resolved.result {
                    SemanticComptimeCallResult::Value(value) => {
                        Ok(ResolvedComptimeArgument::Value(value))
                    }
                    SemanticComptimeCallResult::Type(value) => {
                        Ok(ResolvedComptimeArgument::Type(value))
                    }
                };
            }
            resolve_structured_semantic_type_syntax(provider, root_scope, arena, argument)
                .map(ResolvedComptimeArgument::Type)
        },
    )
}

/// Resolve one parser-structured type without reconstructing or tokenizing its
/// source spelling. Namespace selection and comptime-call policy are shared
/// with [`resolve_semantic_type_syntax`]; only the input adapter differs.
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
    use SemanticResolutionError as E;
    use SemanticTypeSyntaxFailure as F;
    use rue_rir::RirTypeSyntaxNode as R;

    let unknown = || {
        E::Semantic(F::UnknownType {
            syntax: structured_syntax_display(arena, root),
        })
    };
    match arena.node(root).cloned().ok_or_else(unknown)? {
        R::Named(symbol) => {
            let name = arena.symbol(symbol).ok_or_else(unknown)?.as_ref();
            resolve_unqualified_semantic_type(provider, root_scope, name)?.ok_or_else(unknown)
        }
        R::Qualified { path } => {
            let segments = structured_path(arena, path).ok_or_else(unknown)?;
            resolve_qualified_semantic_type(
                provider,
                root_scope,
                &segments,
                structured_syntax_display(arena, root),
            )
        }
        R::Unit => {
            resolve_unqualified_semantic_type(provider, root_scope, "()")?.ok_or_else(unknown)
        }
        R::Never => {
            resolve_unqualified_semantic_type(provider, root_scope, "!")?.ok_or_else(unknown)
        }
        R::Array { element, length } => {
            let element =
                resolve_structured_semantic_type_syntax(provider, root_scope, arena, element)?;
            let length = if let Some(length) = structured_value_syntax(arena, length) {
                lift_provider(provider.resolve_array_length(root_scope, length))?
            } else if let Some(R::ValueCall { name, arguments }) = arena.node(length) {
                let name = arena.symbol(*name).ok_or_else(unknown)?.as_ref();
                let arguments = structured_references(arena, *arguments).ok_or_else(unknown)?;
                let call = resolve_structured_semantic_comptime_call(
                    provider,
                    root_scope,
                    arena,
                    length,
                    vec![name],
                    arguments,
                    SemanticComptimeCallExpectation::Value,
                )?;
                let SemanticComptimeCallResult::Value(value) = call.result else {
                    return Err(unknown());
                };
                lift_provider(provider.array_length_from_value(root_scope, &value))?
            } else {
                return Err(unknown());
            };
            lift_provider(provider.array_type(element, length))
        }
        R::Slice { element } => {
            let syntax = structured_syntax_display(arena, root);
            lift_provider(provider.preflight_slice(root_scope, &syntax))?;
            let element =
                resolve_structured_semantic_type_syntax(provider, root_scope, arena, element)?;
            lift_provider(provider.slice_type(root_scope, &syntax, element))
        }
        R::PointerConst { pointee } => {
            let pointee =
                resolve_structured_semantic_type_syntax(provider, root_scope, arena, pointee)?;
            lift_provider(provider.ptr_const_type(pointee))
        }
        R::PointerMut { pointee } => {
            let pointee =
                resolve_structured_semantic_type_syntax(provider, root_scope, arena, pointee)?;
            lift_provider(provider.ptr_mut_type(pointee))
        }
        R::TypeCall { path, arguments } => {
            let segments = structured_path(arena, path).ok_or_else(unknown)?;
            let arguments = structured_references(arena, arguments).ok_or_else(unknown)?;
            if let [name] = segments.as_slice()
                && let Some(value_arguments) = arguments
                    .iter()
                    .copied()
                    .map(|argument| structured_value_syntax(arena, argument))
                    .collect::<Option<Vec<_>>>()
                && let Some(ty) =
                    lift_provider(provider.builtin_type_call(root_scope, name, &value_arguments))?
            {
                return Ok(ty);
            }
            let call = resolve_structured_semantic_comptime_call(
                provider,
                root_scope,
                arena,
                root,
                segments,
                arguments,
                SemanticComptimeCallExpectation::Type,
            )?;
            let SemanticComptimeCallResult::Type(ty) = call.result else {
                return Err(unknown());
            };
            lift_provider(provider.observe_materialized_type(&ty))?;
            Ok(ty)
        }
        R::AnonymousStruct { .. }
        | R::AnonymousEnum { .. }
        | R::ValueCall { .. }
        | R::Integer(_) => Err(unknown()),
    }
}

pub fn resolve_semantic_type_syntax<S, M, A, K, N, T, V, P>(
    provider: &mut P,
    root_scope: &S,
    syntax: &str,
) -> Result<T, SemanticTypeSyntaxError<P::Abort, P::Failure, A, N>>
where
    M: Clone,
    A: Clone,
    N: Clone,
    P: SemanticTypeSyntaxProvider<S, M, A, K, N, T, V>,
{
    use SemanticResolutionError as E;
    use SemanticTypeSyntaxFailure as F;

    if is_unqualified_name_syntax(syntax)
        && let Some(ty) = resolve_unqualified_semantic_type(provider, root_scope, syntax)?
    {
        return Ok(ty);
    }

    if let Some((element, length)) = crate::parse_array_type_syntax(syntax) {
        let element = resolve_semantic_type_syntax(provider, root_scope, &element)?;
        let length = match &length {
            crate::ArrayLen::Literal(value) => SemanticValueSyntax::Integer(i128::from(*value)),
            crate::ArrayLen::Named(name) => SemanticValueSyntax::from_rendered(name),
        };
        let length = lift_provider(provider.resolve_array_length(root_scope, length))?;
        return lift_provider(provider.array_type(element, length));
    }
    if let Some((pointee, mutability)) = crate::parse_pointer_type_syntax(syntax) {
        let pointee = resolve_semantic_type_syntax(provider, root_scope, &pointee)?;
        return lift_provider(match mutability {
            crate::PtrMutability::Const => provider.ptr_const_type(pointee),
            crate::PtrMutability::Mut => provider.ptr_mut_type(pointee),
        });
    }

    if let Some((call_path, arguments)) = crate::types::parse_type_call_syntax(syntax) {
        let value_arguments = arguments
            .iter()
            .map(|argument| SemanticValueSyntax::from_rendered(argument))
            .collect::<Vec<_>>();
        if !call_path.contains('.')
            && let Some(ty) =
                lift_provider(provider.builtin_type_call(root_scope, &call_path, &value_arguments))?
        {
            return Ok(ty);
        }
        let SemanticComptimeCallResult::Type(ty) = resolve_semantic_comptime_call(
            provider,
            root_scope,
            &call_path,
            &arguments,
            SemanticComptimeCallExpectation::Type,
        )?
        .result
        else {
            unreachable!("type call expectation accepts only type results")
        };
        lift_provider(provider.observe_materialized_type(&ty))?;
        return Ok(ty);
    }

    if let Some(element) = is_slice_syntax(syntax) {
        lift_provider(provider.preflight_slice(root_scope, syntax))?;
        let element = resolve_semantic_type_syntax(provider, root_scope, element)?;
        return lift_provider(provider.slice_type(root_scope, syntax, element));
    }

    if syntax.contains('.') {
        let segments = syntax.split('.').collect::<Vec<_>>();
        return resolve_qualified_semantic_type(provider, root_scope, &segments, Arc::from(syntax));
    }

    Err(E::Semantic(F::UnknownType {
        syntax: Arc::from(syntax),
    }))
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
        bindings: BTreeMap<(&'static str, &'static str), Binding>,
        root_structs: BTreeMap<(&'static str, &'static str), Fact>,
        root_enums: BTreeMap<(&'static str, &'static str), Fact>,
        root_aliases: BTreeMap<(&'static str, &'static str), Fact>,
        module_structs: BTreeMap<(&'static str, &'static str), Fact>,
        module_enums: BTreeMap<(&'static str, &'static str), Fact>,
        module_aliases: BTreeMap<(&'static str, &'static str), Fact>,
        constructors: BTreeMap<(&'static str, &'static str), Head>,
        primitive_error: Option<SemanticProviderError<&'static str, &'static str>>,
        slice_preflight_error: Option<SemanticProviderError<&'static str, &'static str>>,
        allow_qualified_paths: Option<bool>,
        allow_qualified_value_heads: Option<bool>,
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
        fn substituted_type(
            &mut self,
            scope: &&'static str,
            name: &str,
        ) -> FixtureResult<Option<&'static str>> {
            self.call("substitution", scope, name);
            Ok(None)
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
                SemanticValueSyntax::Name(_) | SemanticValueSyntax::Rendered(_) => {
                    Err(SemanticProviderError::Failure("unknown length"))
                }
            }
        }

        fn array_length_from_value(
            &mut self,
            _scope: &&'static str,
            value: &i64,
        ) -> FixtureResult<Option<u64>> {
            Ok(u64::try_from(*value).ok())
        }

        fn array_type(
            &mut self,
            _element: &'static str,
            _length: Option<u64>,
        ) -> FixtureResult<&'static str> {
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

        fn preflight_slice(&mut self, scope: &&'static str, syntax: &str) -> FixtureResult<()> {
            self.call("slice_preflight", scope, syntax);
            if let Some(error) = self.slice_preflight_error.clone() {
                return Err(error);
            }
            Ok(())
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
            Ok(None)
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
                SemanticValueSyntax::Name(syntax) | SemanticValueSyntax::Rendered(syntax) => syntax
                    .parse()
                    .map_err(|_| SemanticProviderError::Failure("unknown value")),
            }
        }

        fn reduce_comptime_call(
            &mut self,
            head: &Head,
            _type_arguments: &[(&'static str, &'static str)],
            _value_arguments: &[(&'static str, i64)],
        ) -> FixtureResult<Option<SemanticComptimeCallResult<&'static str, i64>>> {
            self.calls.push(format!("reduce:{}", head.key));
            Ok(Some(if head.returns_type {
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

        assert_eq!(
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "Thing"),
            Ok("struct")
        );
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
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "api.PublicAlias"),
            Ok("public-alias")
        );
        assert!(matches!(
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "api.PrivateAlias"),
            Err(SemanticResolutionError::Semantic(
                SemanticTypeSyntaxFailure::PrivateItem {
                    kind: SemanticTypeFactKind::Constant,
                    site: "private-site",
                    ..
                }
            ))
        ));
        assert_eq!(
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "Local"),
            Ok("local")
        );
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

        assert_eq!(
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "api.Alias"),
            Ok("alias")
        );
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
                    "slice_preflight:app/main.rue:[i32]",
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
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", syntax).unwrap();
            assert_eq!(fixture.calls, expected, "unexpected trace for '{syntax}'");
        }
    }

    #[test]
    fn structured_and_legacy_adapters_share_resolution_policy() {
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
            let mut legacy = configured();
            let legacy_result =
                resolve_semantic_type_syntax(&mut legacy, &"app/main.rue", syntax).unwrap();

            let (arena, root) = structured_type(syntax);
            let mut structured = configured();
            let structured_result = resolve_structured_semantic_type_syntax(
                &mut structured,
                &"app/main.rue",
                &arena,
                root,
            )
            .unwrap();

            assert_eq!(structured_result, legacy_result, "result for `{syntax}`");
            assert_eq!(structured.calls, legacy.calls, "trace for `{syntax}`");
        }
    }

    #[test]
    fn structured_and_legacy_adapters_share_failure_diagnostics_and_work() {
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

            let mut legacy = configured();
            let legacy_result = resolve_semantic_type_syntax(&mut legacy, &"app/main.rue", syntax);
            assert!(legacy_result.is_err(), "fixture must fail for `{syntax}`");

            let (arena, root) = structured_type(syntax);
            let mut structured = configured();
            let structured_result = resolve_structured_semantic_type_syntax(
                &mut structured,
                &"app/main.rue",
                &arena,
                root,
            );

            assert_eq!(
                structured_result, legacy_result,
                "diagnostic for `{syntax}`"
            );
            assert_eq!(structured.calls, legacy.calls, "work trace for `{syntax}`");
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
    fn slice_preflight_failure_stops_before_child_resolution() {
        let mut fixture = Fixture {
            slice_preflight_error: Some(SemanticProviderError::Failure("preview required")),
            ..Fixture::default()
        };
        assert_eq!(
            resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "[Unknown]"),
            Err(SemanticResolutionError::ProviderFailure("preview required"))
        );
        assert_eq!(fixture.calls, ["slice_preflight:app/main.rue:[Unknown]"]);
    }

    #[test]
    fn value_call_head_restriction_does_not_restrict_qualified_type_arguments() {
        let mut fixture = Fixture {
            allow_qualified_paths: Some(true),
            allow_qualified_value_heads: Some(false),
            ..Fixture::default()
        };
        fixture.constructors.insert(
            ("app/main.rue", "Width"),
            head("width-site", true, false, vec![type_parameter(true)]),
        );
        fixture.bindings.insert(
            ("app/main.rue", "lib"),
            binding("lib", "lib-site", true, "app/main.rue"),
        );
        fixture.module_structs.insert(
            ("lib", "Leaf"),
            fact("leaf", "leaf-site", true, "lib/types.rue"),
        );

        let call = resolve_semantic_comptime_call(
            &mut fixture,
            &"app/main.rue",
            "Width",
            &["lib.Leaf".to_string()],
            SemanticComptimeCallExpectation::Value,
        )
        .expect("the unqualified value callee may receive a qualified type argument");
        assert_eq!(call.result, SemanticComptimeCallResult::Value(2));
        assert!(
            resolve_semantic_comptime_call(
                &mut fixture,
                &"app/main.rue",
                "lib.Width",
                &["lib.Leaf".to_string()],
                SemanticComptimeCallExpectation::Value,
            )
            .is_err(),
            "the value-call head restriction remains independently enforced"
        );
    }

    #[test]
    fn malformed_leaves_and_disallowed_qualified_paths_issue_no_fact_queries() {
        for syntax in ["2", "-3", "true?", "@x"] {
            let mut fixture = Fixture::default();
            assert!(resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", syntax).is_err());
            assert!(fixture.calls.is_empty(), "unexpected trace for '{syntax}'");
        }

        for syntax in ["api.Type", "api.Make(i32)"] {
            let mut fixture = Fixture {
                allow_qualified_paths: Some(false),
                ..Fixture::default()
            };
            assert!(resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", syntax).is_err());
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

            let error = resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", syntax)
                .expect_err("case must fail semantically");
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
            assert_eq!(
                resolve_semantic_type_syntax(&mut fixture, &"app/main.rue", "i32"),
                Err(expected)
            );
            assert_eq!(
                fixture.calls,
                ["substitution:app/main.rue:i32", "primitive:-:i32"]
            );
        }
    }
}
