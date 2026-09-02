//! Skolem identities for the definition-site check of bounded generic
//! functions (spec 6.7:19–6.7:22, preview `interfaces`).
//!
//! A bounded function is checked once by analyzing its body as a
//! specialization whose type arguments are *skolems*: synthetic named
//! nominals, one per comptime type parameter, whose members are exactly the
//! requirements of the parameter's bound (spec 6.7:20). This module owns the
//! identity of a skolem and the two questions the query graph asks about it:
//! which functions get a check, and whether an instance is one.
//!
//! A skolem's [`StableDefinitionKey`] lives in the function's module under a
//! reserved, unspellable name — the same device as the per-module
//! conformance query's source key — so it never collides with a source
//! definition, is identical across cold and warm builds, and decodes on its
//! own: `CompilerBodyDurableSource::nominal` rebuilds the skolem's shape from
//! the function's signature with no body-local state, which is what lets a
//! comptime type constructor applied to a skolem (`Option(T.Element)`) run
//! as an ordinary producer body.

use std::sync::Arc;

use rue_air::Node;

use crate::durable_semantics::{DurableDeclarationPayload, DurableType};
use crate::{
    CanonicalArgumentValue, CanonicalArguments, DurableDeclarationSemantic, FunctionInstanceKey,
    NominalInstanceKey, StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace,
    StableProducerId, TypeInstanceKey,
};

/// The reserved name prefix of every skolem key. A NUL cannot appear in a
/// source identifier, so no declaration can spell it.
const SKOLEM_PREFIX: &str = "\0skolem\0";

/// What a skolem stands for: comptime type parameter `parameter` of the
/// free function `function`, or — with `assoc` — one of that parameter's
/// associated types (`T.Element`), itself an opaque skolem (spec 6.7:20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkolemIdentity {
    pub(crate) function: StableDefinitionKey,
    pub(crate) parameter: Arc<str>,
    pub(crate) assoc: Option<Arc<str>>,
}

impl SkolemIdentity {
    pub(crate) fn parameter(function: &StableDefinitionKey, parameter: &str) -> Self {
        Self {
            function: function.clone(),
            parameter: Arc::from(parameter),
            assoc: None,
        }
    }

    /// The skolem of associated type `name` of this parameter skolem.
    pub(crate) fn assoc(&self, name: &str) -> Self {
        Self {
            function: self.function.clone(),
            parameter: self.parameter.clone(),
            assoc: Some(Arc::from(name)),
        }
    }

    /// The durable key of this skolem: a struct-kind key in the function's
    /// module whose name encodes the function, the parameter, and the
    /// associated type.
    pub(crate) fn key(&self) -> StableDefinitionKey {
        let mut name = String::from(SKOLEM_PREFIX);
        name.push_str(self.function.name());
        name.push('\0');
        name.push_str(&self.parameter);
        if let Some(assoc) = &self.assoc {
            name.push('\0');
            name.push_str(assoc);
        }
        StableDefinitionKey::from_stable_parts(
            self.function.module().clone(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            name,
            None,
        )
    }

    /// Decode a skolem key; `None` for every ordinary definition key.
    pub(crate) fn parse(key: &StableDefinitionKey) -> Option<Self> {
        if key.kind() != StableDefinitionKind::Struct || key.owner().is_some() {
            return None;
        }
        let mut parts = key.name().strip_prefix(SKOLEM_PREFIX)?.split('\0');
        let function = parts.next()?;
        let parameter = parts.next()?;
        let assoc = parts.next();
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            function: StableDefinitionKey::from_stable_parts(
                key.module().clone(),
                StableDefinitionNamespace::Value,
                StableDefinitionKind::Function,
                function,
                None,
            ),
            parameter: Arc::from(parameter),
            assoc: assoc.map(Arc::from),
        })
    }

    /// The name every diagnostic renders the skolem as (spec 6.7:22): the
    /// parameter's name, or `T.Element` for an associated type.
    pub(crate) fn display_name(&self) -> Arc<str> {
        match &self.assoc {
            Some(assoc) => Arc::from(format!("{}.{assoc}", self.parameter)),
            None => self.parameter.clone(),
        }
    }
}

fn key_is_skolem(key: &StableDefinitionKey) -> bool {
    key.kind() == StableDefinitionKind::Struct && key.name().starts_with(SKOLEM_PREFIX)
}

/// Whether a type instance names a skolem anywhere inside it, including
/// through the producer arguments of an anonymous nominal.
fn type_names_skolem(ty: &TypeInstanceKey) -> bool {
    match ty {
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(key)) => key_is_skolem(key),
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(identity)) => {
            match &identity.producer {
                StableProducerId::Definition(_) => false,
                StableProducerId::Function(function) => instance_is_skolem_check(function),
            }
        }
        TypeInstanceKey::Array { element, .. }
        | TypeInstanceKey::Slice { element, .. }
        | TypeInstanceKey::PtrConst(element)
        | TypeInstanceKey::PtrMut(element) => type_names_skolem(element),
        TypeInstanceKey::I8
        | TypeInstanceKey::I16
        | TypeInstanceKey::I32
        | TypeInstanceKey::I64
        | TypeInstanceKey::U8
        | TypeInstanceKey::U16
        | TypeInstanceKey::U32
        | TypeInstanceKey::U64
        | TypeInstanceKey::Bool
        | TypeInstanceKey::Unit
        | TypeInstanceKey::Never
        | TypeInstanceKey::ComptimeType
        | TypeInstanceKey::BuiltinNominal { .. }
        | TypeInstanceKey::Nominal(NominalInstanceKey::Builtin { .. })
        | TypeInstanceKey::Module(_)
        | TypeInstanceKey::GenericParameter(_) => false,
    }
}

/// Whether a body instance belongs to a skolem check (spec 6.7:22): the
/// check itself, or anything instantiated with a skolem on its behalf, such
/// as a comptime type constructor applied to one. Such an instance is
/// analyzed and its diagnostics reported, but nothing it references is
/// scheduled and it never reaches CFG construction or code generation.
pub(crate) fn instance_is_skolem_check(instance: &FunctionInstanceKey) -> bool {
    match instance {
        FunctionInstanceKey::Definition(_) => false,
        FunctionInstanceKey::Specialization { base, arguments } => {
            arguments.types.iter().any(type_names_skolem)
                || arguments.values.iter().any(|value| match value {
                    CanonicalArgumentValue::Type(ty) => type_names_skolem(ty),
                    CanonicalArgumentValue::Function(function) => {
                        instance_is_skolem_check(function)
                    }
                    CanonicalArgumentValue::Integer(_)
                    | CanonicalArgumentValue::Bool(_)
                    | CanonicalArgumentValue::String(_)
                    | CanonicalArgumentValue::Unit => false,
                })
                || instance_is_skolem_check(base)
        }
        FunctionInstanceKey::AnonymousMember { owner, .. } => type_names_skolem(owner),
        FunctionInstanceKey::DropGlue(ty) => type_names_skolem(ty),
    }
}

/// The skolem check of every function among `declarations` that has at
/// least one interface-bounded comptime type parameter and whose comptime
/// parameters are all type parameters (spec 6.7:19). A comptime value
/// parameter has no skolem, and a comptime type constructor (`-> type`) is
/// a producer checked at each instantiation, so neither gets a check
/// (spec 6.7:24). Each check is the function's specialization by the
/// skolems of its comptime parameters, in declaration order.
///
/// Without the interfaces preview only a trusted standard-library module can
/// declare a bound (spec 6.7:25), so `all_modules` is false there and the
/// roots are limited to std's own bounded functions; with the preview every
/// module in the import cone is scanned.
pub(crate) fn skolem_check_roots(
    declarations: &[DurableDeclarationSemantic],
    all_modules: bool,
) -> Vec<FunctionInstanceKey> {
    declarations
        .iter()
        .filter_map(|declaration| {
            if declaration.key.kind() != StableDefinitionKind::Function
                || !(all_modules || declaration.key.module().is_trusted_standard_library())
            {
                return None;
            }
            let DurableDeclarationPayload::Callable {
                parameters, result, ..
            } = &declaration.payload
            else {
                return None;
            };
            if matches!(result, DurableType::ComptimeType) {
                return None;
            }
            let comptime = parameters
                .iter()
                .filter(|parameter| parameter.is_comptime)
                .collect::<Vec<_>>();
            if comptime
                .iter()
                .any(|parameter| !matches!(parameter.ty, DurableType::ComptimeType))
                || comptime.iter().all(|parameter| parameter.bounds.is_empty())
            {
                return None;
            }
            let types = comptime
                .iter()
                .map(|parameter| {
                    TypeInstanceKey::Nominal(NominalInstanceKey::Named(
                        SkolemIdentity::parameter(&declaration.key, &parameter.name).key(),
                    ))
                })
                .collect::<Vec<_>>();
            Some(FunctionInstanceKey::Specialization {
                base: Node::new(FunctionInstanceKey::Definition(declaration.key.clone())),
                arguments: CanonicalArguments {
                    types: types.into(),
                    values: Arc::from([]),
                },
            })
        })
        .collect()
}

/// The note attached to every diagnostic a skolem check of `definition`
/// reports (spec 6.7:22), naming the function and the comptime parameters
/// the check bound to skolems; `None` for an instance that is not that
/// function's own check.
pub(crate) fn skolem_check_note(
    definition: &StableDefinitionKey,
    arguments: &CanonicalArguments,
) -> Option<String> {
    let parameters = arguments
        .types
        .iter()
        .filter_map(|ty| match ty {
            TypeInstanceKey::Nominal(NominalInstanceKey::Named(key)) => SkolemIdentity::parse(key),
            _ => None,
        })
        .filter(|skolem| skolem.assoc.is_none() && skolem.function == *definition)
        .map(|skolem| format!("`{}`", skolem.parameter))
        .collect::<Vec<_>>();
    let name = definition.name();
    match parameters.as_slice() {
        [] => None,
        [parameter] => Some(format!(
            "while checking `{name}` against the bound of parameter {parameter}"
        )),
        [init @ .., last] => Some(format!(
            "while checking `{name}` against the bounds of parameters {} and {last}",
            init.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(name: &str) -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            crate::ModuleId::from_logical_path("main.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            name,
            None,
        )
    }

    #[test]
    fn skolem_keys_round_trip_and_display_as_the_parameter() {
        let render = function("render");
        let skolem = SkolemIdentity::parameter(&render, "T");
        let key = skolem.key();
        assert_eq!(SkolemIdentity::parse(&key), Some(skolem.clone()));
        assert_eq!(skolem.display_name().as_ref(), "T");
        let element = skolem.assoc("Element");
        assert_eq!(SkolemIdentity::parse(&element.key()), Some(element.clone()));
        assert_eq!(element.display_name().as_ref(), "T.Element");
        assert_ne!(key, element.key());
        // Two functions' `T`s are distinct skolems; an ordinary struct is not one.
        assert_ne!(
            key,
            SkolemIdentity::parameter(&function("other"), "T").key()
        );
        assert_eq!(SkolemIdentity::parse(&render), None);
    }

    #[test]
    fn a_check_instance_is_recognized_through_its_arguments() {
        let render = function("render");
        let skolem = TypeInstanceKey::Nominal(NominalInstanceKey::Named(
            SkolemIdentity::parameter(&render, "T").key(),
        ));
        let check = FunctionInstanceKey::Specialization {
            base: Node::new(FunctionInstanceKey::Definition(render.clone())),
            arguments: CanonicalArguments {
                types: Arc::from([skolem.clone()]),
                values: Arc::from([]),
            },
        };
        assert!(instance_is_skolem_check(&check));
        assert!(instance_is_skolem_check(&FunctionInstanceKey::DropGlue(
            Node::new(TypeInstanceKey::Array {
                element: Node::new(skolem),
                len: 2,
            })
        )));
        assert!(!instance_is_skolem_check(&FunctionInstanceKey::Definition(
            render.clone()
        )));
        assert!(!instance_is_skolem_check(
            &FunctionInstanceKey::Specialization {
                base: Node::new(FunctionInstanceKey::Definition(render.clone())),
                arguments: CanonicalArguments {
                    types: Arc::from([TypeInstanceKey::I64]),
                    values: Arc::from([]),
                },
            }
        ));
        assert_eq!(
            skolem_check_note(
                &render,
                &CanonicalArguments {
                    types: Arc::from([
                        TypeInstanceKey::Nominal(NominalInstanceKey::Named(
                            SkolemIdentity::parameter(&render, "T").key(),
                        )),
                        TypeInstanceKey::Nominal(NominalInstanceKey::Named(
                            SkolemIdentity::parameter(&render, "U").key(),
                        )),
                    ]),
                    values: Arc::from([]),
                }
            )
            .as_deref(),
            Some("while checking `render` against the bounds of parameters `T` and `U`")
        );
    }
}
