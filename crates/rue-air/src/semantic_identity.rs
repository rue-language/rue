//! Canonical semantic definition taxonomy shared by live bindings and durable keys.

/// The exhaustive namespace of a semantically bound definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

/// Reviewable inventory of every stable semantic namespace.
pub const STABLE_DEFINITION_NAMESPACES: &[StableDefinitionNamespace] = &[
    StableDefinitionNamespace::Value,
    StableDefinitionNamespace::Type,
    StableDefinitionNamespace::Destructor,
    StableDefinitionNamespace::Method,
];

// One reviewable source generates the stable kind enum, inventory, namespace,
// and ownership policy. Adding a kind cannot compile until all taxonomy fields
// are supplied here.
macro_rules! stable_definition_kind_schema {
    ($consumer:ident) => {
        $consumer! {
            Function, Value, true, false;
            Struct, Type, false, false;
            Enum, Type, false, false;
            ValueConst, Value, false, false;
            ModuleBinding, Value, false, false;
            Destructor, Destructor, true, true;
            Method, Method, true, true;
            AssociatedFunction, Method, true, true;
        }
    };
}

macro_rules! define_stable_definition_kind_schema {
    ($( $kind:ident, $namespace:ident, $owns_body:literal, $requires_owner:literal; )*) => {
        /// The exhaustive kind of a semantically bound definition.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StableDefinitionKind {
            $( $kind, )*
        }

        /// Reviewable inventory of every stable semantic definition kind.
        pub const STABLE_DEFINITION_KINDS: &[StableDefinitionKind] = &[
            $( StableDefinitionKind::$kind, )*
        ];

        impl StableDefinitionKind {
            /// The only namespace in which this kind can be issued.
            pub const fn namespace(self) -> StableDefinitionNamespace {
                match self {
                    $( Self::$kind => StableDefinitionNamespace::$namespace, )*
                }
            }

            /// Whether this definition owns an executable semantic body.
            pub const fn owns_body(self) -> bool {
                match self {
                    $( Self::$kind => $owns_body, )*
                }
            }

            /// Whether this definition must name an owning nominal type.
            pub const fn requires_owner(self) -> bool {
                match self {
                    $( Self::$kind => $requires_owner, )*
                }
            }
        }
    };
}

stable_definition_kind_schema!(define_stable_definition_kind_schema);

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
