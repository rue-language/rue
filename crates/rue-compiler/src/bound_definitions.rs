//! Stable source-definition identities issued only after semantic binding.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::ModuleId;

/// Durable-key name for the canonical semantic namespace taxonomy.
pub type StableDefinitionNamespace = rue_air::StableDefinitionNamespace;

/// Durable-key name for the canonical semantic kind taxonomy.
pub type StableDefinitionKind = rue_air::StableDefinitionKind;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedTypeKey {
    module: ModuleId,
    kind: StableDefinitionKind,
    name: Arc<str>,
}

impl StableNamedTypeKey {
    pub fn module(&self) -> &ModuleId {
        &self.module
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.kind
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn shared_name(&self) -> &Arc<str> {
        &self.name
    }
}

#[derive(Debug)]
struct StableDefinitionIdentity {
    module: ModuleId,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: Arc<str>,
    owner: Option<StableNamedTypeKey>,
    hash_accelerator: [u8; 16],
}

/// Immutable durable identity for one bound definition.
///
/// These keys are copied through recursive type/function identities and then
/// hashed repeatedly by the query runtime. Sharing the exact field payload
/// keeps clones constant-size, while the cached collision-resistant
/// accelerator avoids re-hashing every module/name string at each lookup.
/// Equality and ordering remain authoritative over the complete fields: the
/// accelerator is only a bucket selector and cannot conflate distinct keys.
/// It is recomputed at issuance and is not a durable or serialized identity.
#[derive(Clone)]
pub struct StableDefinitionKey(Arc<StableDefinitionIdentity>);

/// Bucket-selector digest for one issued definition identity.
///
/// This is a hash-table accelerator, never a durable or serialized value:
/// [`StableDefinitionKey`]'s `PartialEq` and `Ord` remain authoritative over
/// the complete fields, so a collision here costs one extra field comparison
/// in a bucket and can never conflate distinct keys. That makes a
/// cryptographic digest unnecessary — issuance previously ran SHA-256 over
/// every module, name, and owner string, which measured 1.9% of a fresh
/// Lattice compile purely to select buckets. `StableHasher` is the
/// repository's fixed-key, byte-order-independent mixer, so the accelerator
/// stays deterministic across processes at a fraction of the cost.
fn definition_hash_accelerator(
    module: &ModuleId,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: &Arc<str>,
    owner: &Option<StableNamedTypeKey>,
) -> [u8; 16] {
    let mut hasher = rue_query::StableHasher::new();
    hasher.write(b"rue.stable-definition-key\0v2\0stable-hasher\0");
    module.hash(&mut hasher);
    namespace.hash(&mut hasher);
    kind.hash(&mut hasher);
    name.hash(&mut hasher);
    owner.hash(&mut hasher);
    hasher.finish128().to_u128().to_le_bytes()
}

impl StableDefinitionKey {
    pub(crate) fn from_stable_parts(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: impl Into<Arc<str>>,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> Self {
        let owner = owner.map(|(kind, name)| StableNamedTypeKey {
            module: module.clone(),
            kind,
            name,
        });
        let name = name.into();
        let hash_accelerator = definition_hash_accelerator(&module, namespace, kind, &name, &owner);
        Self(Arc::new(StableDefinitionIdentity {
            module,
            namespace,
            kind,
            name,
            owner,
            hash_accelerator,
        }))
    }

    pub fn module(&self) -> &ModuleId {
        &self.0.module
    }
    pub fn namespace(&self) -> StableDefinitionNamespace {
        self.0.namespace
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.0.kind
    }
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub(crate) fn shared_name(&self) -> &Arc<str> {
        &self.0.name
    }
    pub fn owner(&self) -> Option<&StableNamedTypeKey> {
        self.0.owner.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: impl Into<Arc<str>>,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> Self {
        Self::from_stable_parts(module, namespace, kind, name, owner)
    }
}

impl fmt::Debug for StableDefinitionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StableDefinitionKey")
            .field("module", &self.0.module)
            .field("namespace", &self.0.namespace)
            .field("kind", &self.0.kind)
            .field("name", &self.0.name)
            .field("owner", &self.0.owner)
            .finish()
    }
}

impl PartialEq for StableDefinitionKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.module == other.0.module
                && self.0.namespace == other.0.namespace
                && self.0.kind == other.0.kind
                && self.0.name == other.0.name
                && self.0.owner == other.0.owner)
    }
}

impl Eq for StableDefinitionKey {}

impl PartialOrd for StableDefinitionKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StableDefinitionKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Stable keys propagate by cloning this immutable identity. Ordered
        // maps commonly compare a lookup key with that exact clone, so match
        // equality's constant-time path before walking the shared strings.
        if Arc::ptr_eq(&self.0, &other.0) {
            return std::cmp::Ordering::Equal;
        }
        (
            &self.0.module,
            self.0.namespace,
            self.0.kind,
            &self.0.name,
            &self.0.owner,
        )
            .cmp(&(
                &other.0.module,
                other.0.namespace,
                other.0.kind,
                &other.0.name,
                &other.0.owner,
            ))
    }
}

impl Hash for StableDefinitionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.0.hash_accelerator);
    }
}

#[cfg(test)]
mod tests {
    use ahash::AHashMap;
    use std::hash::Hasher;
    use std::sync::Arc;

    use rue_span::FileId;

    use super::*;
    use crate::revisioned_query_database::test_support::production_declarations;
    use crate::{SourceMetadata, SourceSnapshot};

    #[derive(Default)]
    struct ByteCountingHasher {
        bytes: usize,
    }

    impl Hasher for ByteCountingHasher {
        fn finish(&self) -> u64 {
            self.bytes as u64
        }

        fn write(&mut self, bytes: &[u8]) {
            self.bytes += bytes.len();
        }
    }

    #[test]
    fn stable_definition_hashing_is_constant_size_and_collision_safe() {
        let module = ModuleId::from_logical_path("a/very/long/module/path").unwrap();
        let first = StableDefinitionKey::for_test(
            module.clone(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "a_very_long_function_name",
            None,
        );
        let cloned = first.clone();
        assert!(Arc::ptr_eq(&first.0, &cloned.0));
        assert_eq!(first.cmp(&cloned), std::cmp::Ordering::Equal);

        let independently_issued_equal = StableDefinitionKey::for_test(
            module.clone(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "a_very_long_function_name",
            None,
        );
        assert!(!Arc::ptr_eq(&first.0, &independently_issued_equal.0));
        assert_eq!(first, independently_issued_equal);
        assert_eq!(
            first.cmp(&independently_issued_equal),
            std::cmp::Ordering::Equal
        );

        let mut hasher = ByteCountingHasher::default();
        first.hash(&mut hasher);
        assert_eq!(hasher.bytes, 16);

        let mut second = StableDefinitionKey::for_test(
            module,
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "another_function",
            None,
        );
        Arc::get_mut(&mut second.0).unwrap().hash_accelerator = first.0.hash_accelerator;
        assert_ne!(first, second);
        assert_ne!(first.cmp(&second), std::cmp::Ordering::Equal);
        assert_eq!(first.cmp(&second), second.cmp(&first).reverse());

        let mut colliding = AHashMap::new();
        colliding.insert(first, 1);
        colliding.insert(second, 2);
        assert_eq!(colliding.len(), 2);
    }

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<AHashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<AHashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    const PROGRAM: &str = r#"
        struct Resource {
            value: i32,
            fn get(self) -> i32 { self.value }
            fn make() -> Resource { Resource { value: 0 } }
        }
        enum Choice { None, Some(i32) }
        const LIMIT: i32 = 4;
        drop fn Resource(self) {}
        fn main() -> i32 { Resource.make().get() + LIMIT }
    "#;

    #[test]
    fn durable_declaration_export_is_relocation_and_order_stable() {
        let first = snapshot(
            &[
                (
                    9,
                    "/old/z.rue",
                    "z.rue",
                    "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                ),
                (2, "/old/main.rue", "main.rue", PROGRAM),
            ],
            2,
        );
        let moved = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", PROGRAM),
                (
                    4,
                    "/new/z.rue",
                    "z.rue",
                    "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                ),
            ],
            71,
        );
        let first = production_declarations(&first);
        let moved = production_declarations(&moved);
        assert_eq!(
            first, moved,
            "durable declaration semantics must not observe physical relocation or batch order"
        );
        assert!(first.iter().any(|record| matches!(
            record.payload,
            crate::DurableDeclarationPayload::Const {
                value: crate::DurableConstValue::Function(_),
                ..
            }
        )));
    }

    #[test]
    fn durable_export_keeps_a_real_root_file_zero_nominal_distinct_from_builtins() {
        let source = snapshot(
            &[(
                0,
                "/workspace/main.rue",
                "main.rue",
                "struct Root { value: i32 } fn make() -> Root { Root { value: 0 } }",
            )],
            0,
        );
        let declarations = production_declarations(&source);
        let root = declarations
            .iter()
            .find(|declaration| declaration.key.name() == "Root")
            .expect("root nominal must be durably exported")
            .key
            .clone();
        let make = declarations
            .iter()
            .find(|declaration| declaration.key.name() == "make")
            .expect("function returning the root nominal must be durably exported");
        assert!(matches!(
            &make.payload,
            crate::DurableDeclarationPayload::Callable { result, .. }
                if result == &crate::DurableType::Nominal(root)
        ));
    }

    #[test]
    fn durable_module_binding_export_joins_logical_identity_after_physical_relocation() {
        let source = snapshot(
            &[
                (
                    0,
                    "/relocated/project/main.rue",
                    "main.rue",
                    "const imported = @import(\"lib.rue\"); fn main() -> i32 { 0 }",
                ),
                (
                    9,
                    "/relocated/project/lib.rue",
                    "lib.rue",
                    "fn value() -> i32 { 1 }",
                ),
            ],
            0,
        );
        let declarations = production_declarations(&source);
        let binding = declarations
            .iter()
            .find(|declaration| {
                declaration.key.kind() == StableDefinitionKind::ModuleBinding
                    && declaration.key.name() == "imported"
            })
            .expect("the module binding must be durably exported");
        assert!(matches!(
            &binding.payload,
            crate::DurableDeclarationPayload::ModuleBinding { target }
                if target == &ModuleId::from_logical_path("lib.rue").unwrap()
        ));
    }

    #[test]
    fn durable_member_export_joins_same_named_members_through_their_stable_owner() {
        const MEMBERS: &str = r#"
            struct Alpha {
                fn shared(self) -> i32 { 1 }
                fn make() -> i32 { 2 }
            }
            struct Beta {
                fn shared(self) -> bool { true }
                fn make() -> bool { false }
            }
            fn main() {}
        "#;
        let first = snapshot(
            &[
                (9, "/old/z.rue", "z.rue", "fn helper() {}"),
                (2, "/old/main.rue", "main.rue", MEMBERS),
            ],
            2,
        );
        let moved = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", MEMBERS),
                (4, "/new/z.rue", "z.rue", "fn helper() {}"),
            ],
            71,
        );
        let first = production_declarations(&first);
        let moved = production_declarations(&moved);
        assert_eq!(first, moved);

        let members = first
            .iter()
            .filter(|record| {
                matches!(
                    record.key.kind(),
                    StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                )
            })
            .map(|record| {
                (
                    record.key.kind(),
                    record.key.name().to_owned(),
                    record.key.owner().unwrap().name().to_owned(),
                    record.payload.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 4);
        for name in ["make", "shared"] {
            let same_name = members
                .iter()
                .filter(|member| member.1 == name)
                .collect::<Vec<_>>();
            assert_eq!(same_name.len(), 2);
            assert_ne!(same_name[0].2, same_name[1].2);
            assert_ne!(same_name[0].3, same_name[1].3);
        }
    }

    #[test]
    fn rename_and_module_move_change_only_the_stable_identity_components() {
        let keys = |entries: &[(u32, &str, &str, &str)], root: u32| {
            production_declarations(&snapshot(entries, root))
                .iter()
                .map(|record| record.key.clone())
                .collect::<Vec<_>>()
        };
        let original = keys(&[(1, "/a.rue", "a.rue", "fn main() {}")], 1);
        let renamed = keys(&[(7, "/a.rue", "a.rue", "fn renamed() {}")], 7);
        let moved = keys(&[(8, "/b.rue", "b.rue", "fn main() {}")], 8);
        assert_ne!(original, renamed);
        assert_ne!(original, moved);
        assert_eq!(original[0].name(), "main");
        assert_eq!(moved[0].module().as_str(), "b.rue");
    }
}
