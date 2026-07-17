//! Canonical semantic definition taxonomy shared by live bindings and durable keys.

/// The exhaustive namespace of a semantically bound definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

/// The exhaustive kind of a semantically bound definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
    Destructor,
    Method,
    AssociatedFunction,
}

impl StableDefinitionKind {
    /// The only namespace in which this kind can be issued.
    pub const fn namespace(self) -> StableDefinitionNamespace {
        match self {
            Self::Function | Self::ValueConst | Self::ModuleBinding => {
                StableDefinitionNamespace::Value
            }
            Self::Struct | Self::Enum => StableDefinitionNamespace::Type,
            Self::Destructor => StableDefinitionNamespace::Destructor,
            Self::Method | Self::AssociatedFunction => StableDefinitionNamespace::Method,
        }
    }

    /// Whether this definition owns an executable semantic body.
    pub const fn owns_body(self) -> bool {
        matches!(
            self,
            Self::Function | Self::Destructor | Self::Method | Self::AssociatedFunction
        )
    }

    /// Whether this definition must name an owning nominal type.
    pub const fn requires_owner(self) -> bool {
        matches!(
            self,
            Self::Destructor | Self::Method | Self::AssociatedFunction
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_declares_every_kind_namespace_and_owner_shape_once() {
        use StableDefinitionKind as K;
        use StableDefinitionNamespace as N;

        let cases = [
            (K::Function, N::Value, true, false),
            (K::Struct, N::Type, false, false),
            (K::Enum, N::Type, false, false),
            (K::ValueConst, N::Value, false, false),
            (K::ModuleBinding, N::Value, false, false),
            (K::Destructor, N::Destructor, true, true),
            (K::Method, N::Method, true, true),
            (K::AssociatedFunction, N::Method, true, true),
        ];
        for (kind, namespace, owns_body, requires_owner) in cases {
            assert_eq!(kind.namespace(), namespace);
            assert_eq!(kind.owns_body(), owns_body);
            assert_eq!(kind.requires_owner(), requires_owner);
        }
    }
}
