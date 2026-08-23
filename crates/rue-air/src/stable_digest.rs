//! Stable digests for durable AIR identities.
//!
//! During the RUE-1091 flip, both the semantic epoch and the provider-era
//! identity pool must derive anonymous nominal display names through this
//! module. Keeping the stable-content encoding here prevents the two paths
//! from drifting while the provider replaces the epoch.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use crate::AnonymousNominalKey;

/// A fixed-seed FNV-1a 128-bit hasher.
///
/// Unlike the standard-library `DefaultHasher`, its algorithm and seed are
/// pinned in source, so the digest of one byte stream is identical across every
/// compile of the same program — warm, fresh, or differently scheduled. It is
/// used only to spell stable anonymous-symbol names; it is not a cryptographic
/// hash.
struct StableFnv1a128(u128);

impl StableFnv1a128 {
    /// The 128-bit FNV-1a offset basis.
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    /// The 128-bit FNV-1a prime.
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn digest(self) -> u128 {
        self.0
    }
}

impl Hasher for StableFnv1a128 {
    fn finish(&self) -> u64 {
        // Truncation is never used for identity; `digest()` reads all 128 bits.
        self.0 as u64
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u128::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
}

/// The stable-content string of one INSTALLED definition endpoint:
/// `D\u{1}{module_path}\u{1}{name}\u{1}{owner}\u{1}{kind}`, with an absent
/// owner rendered as the empty string.
///
/// This is the single assembly of the definition-component format both
/// relocation paths feed into [`stable_anonymous_identity_digest`]: the
/// semantic epoch's `stable_definition_symbol_component` (`anon_structs.rs`,
/// installed-endpoint arm) and the provider durable adapter that formats the
/// same four parts from a durable definition key. Every byte here is
/// digest-critical — two paths spelling the same producer must hash the same
/// content. The epoch's session-local `d\u{1}…` fallback is a separate,
/// deliberately non-durable namespace and is not assembled here.
pub fn stable_definition_component(
    module_path: &str,
    name: &str,
    owner: Option<&str>,
    kind: u8,
) -> String {
    format!(
        "D\u{1}{module_path}\u{1}{name}\u{1}{}\u{1}{kind}",
        owner.unwrap_or("")
    )
}

/// The stable-content string of one INSTALLED module endpoint:
/// `M\u{1}{module_path}`. The module analog of
/// [`stable_definition_component`], shared by the same two relocation paths;
/// the epoch's session-local `m\u{1}…` fallback is not assembled here.
pub fn stable_module_component(module_path: &str) -> String {
    format!("M\u{1}{module_path}")
}

/// Computes the canonical digest of a stable-content anonymous nominal key.
///
/// Callers must first relocate any session-local definition and module tokens
/// to their stable string content. This function is the single encoding and
/// digest path shared by the semantic epoch and the provider-era identity pool
/// during the RUE-1091 flip.
pub fn stable_anonymous_identity_digest(identity: &AnonymousNominalKey<String, String>) -> u128 {
    let mut hasher = StableFnv1a128::new();
    identity.durable_encode(&mut hasher);
    hasher.digest()
}

/// Writes the durable byte stream of one identity node.
///
/// This exists so that the name an anonymous nominal is spelled with is a
/// function of an encoding written down here, rather than of whatever
/// `#[derive(Hash)]` happens to emit for the identity types. Those two used to
/// be the same thing, which made the symbol scheme fragile in a way nothing
/// announced: reordering a field, inserting an enum variant, or giving an edge
/// a cheaper `Hash` renames every anonymous nominal in every program, silently,
/// from a change that looks local. RUE-1763 hit exactly that -- a digest-carrying
/// edge wrapper made `Hash` an accelerator, which is right for map probes and
/// wrong for a name.
///
/// The stream reproduces what the derive produced before, byte for byte, so no
/// symbol moves: a variant writes its zero-based index the way the derive wrote
/// its discriminant, then its fields in declaration order. Fields that cannot
/// reach an identity edge delegate to `Hash`, which keeps this to the types
/// that actually carry the recursion. `durable_encode_agrees_with_the_derived_hash`
/// pins the equivalence over every variant.
trait DurableEncode {
    fn durable_encode<H: Hasher>(&self, state: &mut H);
}

/// The derive writes an enum's discriminant as an `isize` before its fields,
/// and for a default-repr enum that discriminant is the zero-based variant
/// index.
fn encode_variant<H: Hasher>(state: &mut H, index: isize) {
    state.write_isize(index);
}

/// The derive writes a slice's length before its elements, through
/// `Hasher::write_length_prefix`. That method is still unstable, so this calls
/// what its default body calls; `StableFnv1a128` overrides neither, so the two
/// reach `write` with the same bytes.
fn encode_slice<H: Hasher, T: DurableEncode>(state: &mut H, items: &[T]) {
    state.write_usize(items.len());
    for item in items {
        item.durable_encode(state);
    }
}

impl DurableEncode for AnonymousNominalKey<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.producer.durable_encode(state);
        self.anchor.hash(state);
        self.arguments.durable_encode(state);
    }
}

impl DurableEncode for crate::StableProducerId<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Definition(definition) => {
                encode_variant(state, 0);
                definition.hash(state);
            }
            Self::Function(function) => {
                encode_variant(state, 1);
                function.durable_encode(state);
            }
        }
    }
}

impl DurableEncode for crate::CanonicalArguments<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        encode_slice(state, &self.types);
        encode_slice(state, &self.values);
    }
}

impl DurableEncode for crate::CanonicalArgumentValue<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Integer(value) => {
                encode_variant(state, 0);
                value.hash(state);
            }
            Self::Bool(value) => {
                encode_variant(state, 1);
                value.hash(state);
            }
            Self::Type(value) => {
                encode_variant(state, 2);
                value.durable_encode(state);
            }
            Self::Function(value) => {
                encode_variant(state, 3);
                value.durable_encode(state);
            }
            Self::Unit => encode_variant(state, 4),
            Self::String(value) => {
                encode_variant(state, 5);
                value.hash(state);
            }
        }
    }
}

impl DurableEncode for crate::NominalInstanceKey<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Builtin { kind, name } => {
                encode_variant(state, 0);
                kind.hash(state);
                name.hash(state);
            }
            Self::Named(key) => {
                encode_variant(state, 1);
                key.hash(state);
            }
            Self::Anonymous(key) => {
                encode_variant(state, 2);
                key.durable_encode(state);
            }
        }
    }
}

impl DurableEncode for crate::FunctionInstanceKey<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Definition(key) => {
                encode_variant(state, 0);
                key.hash(state);
            }
            Self::Specialization { base, arguments } => {
                encode_variant(state, 1);
                base.durable_encode(state);
                arguments.durable_encode(state);
            }
            Self::AnonymousMember { owner, member } => {
                encode_variant(state, 2);
                owner.durable_encode(state);
                member.hash(state);
            }
            Self::DropGlue(owner) => {
                encode_variant(state, 3);
                owner.durable_encode(state);
            }
        }
    }
}

impl DurableEncode for crate::TypeInstanceKey<String, String> {
    fn durable_encode<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::I8 => encode_variant(state, 0),
            Self::I16 => encode_variant(state, 1),
            Self::I32 => encode_variant(state, 2),
            Self::I64 => encode_variant(state, 3),
            Self::U8 => encode_variant(state, 4),
            Self::U16 => encode_variant(state, 5),
            Self::U32 => encode_variant(state, 6),
            Self::U64 => encode_variant(state, 7),
            Self::Bool => encode_variant(state, 8),
            Self::Unit => encode_variant(state, 9),
            Self::Never => encode_variant(state, 10),
            Self::ComptimeType => encode_variant(state, 11),
            Self::BuiltinNominal { kind, name } => {
                encode_variant(state, 12);
                kind.hash(state);
                name.hash(state);
            }
            Self::Nominal(nominal) => {
                encode_variant(state, 13);
                nominal.durable_encode(state);
            }
            Self::Array { element, len } => {
                encode_variant(state, 14);
                element.durable_encode(state);
                len.hash(state);
            }
            Self::Slice { element, name } => {
                encode_variant(state, 15);
                element.durable_encode(state);
                name.hash(state);
            }
            Self::PtrConst(element) => {
                encode_variant(state, 16);
                element.durable_encode(state);
            }
            Self::PtrMut(element) => {
                encode_variant(state, 17);
                element.durable_encode(state);
            }
            Self::Module(module) => {
                encode_variant(state, 18);
                module.hash(state);
            }
            Self::GenericParameter(index) => {
                encode_variant(state, 19);
                index.hash(state);
            }
        }
    }
}

/// Infix that separates a base digest from its collision-disambiguating
/// ordinal. Its `$` is not a hex digit and not a legal identifier character —
/// the same generated-name punctuation `named_nominal_source_symbol` already
/// qualifies with — so a disambiguated component can never be re-read as a bare
/// 32-hex digest, and [`crate::mangle_symbol_component`] escapes it to a
/// portable object-symbol byte like every other generated-name character.
const ANONYMOUS_SYMBOL_DISAMBIGUATOR: &str = "$c";

/// Spell the digest component of one anonymous symbol.
///
/// `ordinal` is `None` for the overwhelmingly common case — a digest owned by
/// exactly one producer-nominal identity — and the component is then the bare
/// 32-hex digest, byte-identical to the pre-RUE-1114 spelling. It is `Some(i)`
/// for every member of a verified collision class, where `i` is the member's
/// rank under [`anonymous_symbol_ordinals`].
///
/// EVERY member of a collision class carries an explicit ordinal: no member
/// silently keeps the unqualified spelling, so the scheme has no
/// first-registrant (nor minimum-registrant) winner. The component stays
/// bounded — 32 hex digits plus a two-byte infix plus at most ten decimal
/// digits — because an ordinal cannot exceed the size of the reached set.
pub fn stable_anonymous_symbol_component(digest: u128, ordinal: Option<u32>) -> String {
    match ordinal {
        None => format!("{digest:032x}"),
        Some(ordinal) => format!("{digest:032x}{ANONYMOUS_SYMBOL_DISAMBIGUATOR}{ordinal}"),
    }
}

/// Deterministically disambiguate verified anonymous symbol digest collisions
/// over one COMPLETE set of reached anonymous identities (RUE-1114).
///
/// Input is `(digest, identity)` for every anonymous nominal in the set, so a
/// caller that narrows digests (the forced-collision test hooks) feeds the same
/// rule as production. Identities are the request-independent stable-content
/// form: their total order is a function of module paths, definition names,
/// structural anchors, and canonical arguments only — never of a session-issued
/// token, a traversal position, or an arrival order. That is what makes the
/// result reproducible rather than merely deterministic within one process.
///
/// A digest owned by one identity is absent from the result (bare spelling
/// retained). A digest owned by two or more DISTINCT identities yields an entry
/// per member, ranked by that stable total order. Ranking is therefore
/// order-independent by construction: the input is folded into ordered sets
/// before any ordinal exists, so permuting the input — or discovering the same
/// set across separate body transactions, or recompiling incrementally — cannot
/// move a member.
///
/// The exact identity, not the digest, remains the sole semantic authority; this
/// only decides how a collision class is SPELLED.
pub fn anonymous_symbol_ordinals<I>(
    entries: I,
) -> BTreeMap<AnonymousNominalKey<String, String>, u32>
where
    I: IntoIterator<Item = (u128, AnonymousNominalKey<String, String>)>,
{
    let mut classes: BTreeMap<u128, BTreeSet<AnonymousNominalKey<String, String>>> =
        BTreeMap::new();
    for (digest, identity) in entries {
        classes.entry(digest).or_default().insert(identity);
    }
    let mut ordinals = BTreeMap::new();
    for members in classes.into_values() {
        if members.len() < 2 {
            continue;
        }
        for (ordinal, member) in members.into_iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .expect("a reached anonymous collision class cannot exceed u32::MAX members");
            ordinals.insert(member, ordinal);
        }
    }
    ordinals
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use rue_rir::{RirStructuralAnchor, RirStructuralPathSegment};

    use std::hash::Hash as _;

    use super::{
        StableFnv1a128, anonymous_symbol_ordinals, stable_anonymous_identity_digest,
        stable_anonymous_symbol_component, stable_definition_component, stable_module_component,
    };
    use crate::{
        AnonymousNominalKey, AnonymousNominalKind, CanonicalArgumentValue, CanonicalArguments,
        StableProducerId,
    };

    /// A stable-content anonymous identity distinguished only by its producer
    /// definition component and site anchor — the two coordinates the RUE-1114
    /// total order is built on.
    fn identity(
        kind: AnonymousNominalKind,
        producer: &str,
        site: u32,
    ) -> AnonymousNominalKey<String, String> {
        AnonymousNominalKey {
            kind,
            producer: StableProducerId::Definition(stable_definition_component(
                "pkg/m.rue",
                producer,
                None,
                3,
            )),
            anchor: RirStructuralAnchor::new(vec![
                RirStructuralPathSegment::Body,
                RirStructuralPathSegment::AnonymousType(site),
            ]),
            arguments: CanonicalArguments {
                types: Arc::new([]),
                values: Arc::new([]),
            },
        }
    }

    /// The four distinct sites every disambiguation test forces onto one
    /// digest: two kinds crossed with two producers, so struct/enum parity and
    /// producer separation are both covered by one class.
    fn colliding_sites() -> Vec<AnonymousNominalKey<String, String>> {
        vec![
            identity(AnonymousNominalKind::Struct, "First", 0),
            identity(AnonymousNominalKind::Enum, "First", 0),
            identity(AnonymousNominalKind::Struct, "Second", 1),
            identity(AnonymousNominalKind::Enum, "Second", 1),
        ]
    }

    /// Spell every member of a forced-collision set, so a test can compare
    /// whole symbol tables rather than individual ordinals.
    fn symbol_table(
        forced: u128,
        sites: &[AnonymousNominalKey<String, String>],
    ) -> BTreeMap<AnonymousNominalKey<String, String>, String> {
        let ordinals = anonymous_symbol_ordinals(sites.iter().map(|site| (forced, site.clone())));
        sites
            .iter()
            .map(|site| {
                (
                    site.clone(),
                    stable_anonymous_symbol_component(forced, ordinals.get(site).copied()),
                )
            })
            .collect()
    }

    /// A digest owned by exactly one identity keeps the bare 32-hex spelling:
    /// the disambiguation scheme is inert for every collision-free program, so
    /// no existing symbol table moves.
    #[test]
    fn a_solitary_digest_owner_keeps_the_bare_spelling() {
        let sites = colliding_sites();
        let ordinals = anonymous_symbol_ordinals(
            sites
                .iter()
                .enumerate()
                .map(|(index, site)| (index as u128, site.clone())),
        );
        assert!(ordinals.is_empty(), "distinct digests need no ordinal");
        assert_eq!(
            stable_anonymous_symbol_component(0x2a, None),
            "0000000000000000000000000000002a"
        );
    }

    /// Every member of a verified collision class is spelled distinctly, and
    /// NO member keeps the unqualified spelling — the property that rules out
    /// first-registrant (and minimum-registrant) winners.
    #[test]
    fn a_collision_class_spells_every_member_distinctly_and_explicitly() {
        let forced = 0x1114;
        let sites = colliding_sites();
        let table = symbol_table(forced, &sites);
        let bare = stable_anonymous_symbol_component(forced, None);
        assert_eq!(table.len(), sites.len());
        assert_eq!(
            table.values().collect::<BTreeSet<_>>().len(),
            sites.len(),
            "two colliding sites share a symbol"
        );
        for symbol in table.values() {
            assert_ne!(*symbol, bare, "a collision member kept the bare spelling");
            assert!(symbol.starts_with(&bare));
            assert!(symbol.len() <= bare.len() + "$c".len() + 10, "unbounded");
        }
        assert_eq!(
            table.values().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                format!("{bare}$c0"),
                format!("{bare}$c1"),
                format!("{bare}$c2"),
                format!("{bare}$c3"),
            ])
        );
    }

    /// Order independence, stated over the observable artifact: the symbol
    /// table is identical under every permutation of discovery order. This is
    /// the reproducible-build property — input order, scheduling, and
    /// cold/warm reuse all reduce to a permutation of the same reached set.
    #[test]
    fn collision_spelling_is_independent_of_discovery_order() {
        let forced = 0x1114;
        let sites = colliding_sites();
        let expected = symbol_table(forced, &sites);

        let mut permutations = 0;
        // All 24 orderings of the four-member class, enumerated through the
        // factorial number system: pick the a-th remaining site, then the b-th,
        // and so on. No permutation may change a single spelling.
        for a in 0..4 {
            for b in 0..3 {
                for c in 0..2 {
                    let mut permutation = sites.clone();
                    let first = permutation.remove(a);
                    let second = permutation.remove(b);
                    let third = permutation.remove(c);
                    let rest = permutation.remove(0);
                    let permuted = vec![first, second, third, rest];
                    assert_eq!(
                        symbol_table(forced, &permuted),
                        expected,
                        "discovery order changed the symbol table"
                    );
                    permutations += 1;
                }
            }
        }
        assert_eq!(permutations, 24);
    }

    /// Stability across recompiles: recomputing the plan from the same reached
    /// set — the incremental warm/successor-revision case — reproduces the same
    /// table, and a member REMOVED from the set never renames a member that
    /// stays, as long as its own class membership is unchanged.
    #[test]
    fn collision_spelling_is_stable_across_recomputation_and_unrelated_edits() {
        let forced = 0x1114;
        let sites = colliding_sites();
        let first = symbol_table(forced, &sites);
        assert_eq!(symbol_table(forced, &sites), first);

        // An unrelated site joins the reached set under its OWN digest: it is
        // not a class member, so no colliding member is renamed.
        let mut widened: Vec<(u128, AnonymousNominalKey<String, String>)> =
            sites.iter().map(|site| (forced, site.clone())).collect();
        let unrelated = identity(AnonymousNominalKind::Struct, "Unrelated", 7);
        widened.push((forced ^ 0xff, unrelated.clone()));
        let widened = anonymous_symbol_ordinals(widened);
        assert!(!widened.contains_key(&unrelated));
        for site in &sites {
            assert_eq!(
                stable_anonymous_symbol_component(forced, widened.get(site).copied()),
                first[site]
            );
        }
    }

    /// Presenting the same identity twice is legitimate reuse, not a
    /// collision: the class is deduplicated by exact key before ranking.
    #[test]
    fn repeating_one_identity_is_not_a_collision() {
        let site = identity(AnonymousNominalKind::Struct, "First", 0);
        let ordinals = anonymous_symbol_ordinals([(7, site.clone()), (7, site.clone()), (7, site)]);
        assert!(ordinals.is_empty());
    }

    /// Ordinals are class-local: two separate collision classes each rank from
    /// zero, so a symbol is `(digest, ordinal)` and never a global counter that
    /// a third class could shift.
    #[test]
    fn ordinals_are_local_to_their_collision_class() {
        let left = colliding_sites();
        let right = vec![
            identity(AnonymousNominalKind::Struct, "Third", 2),
            identity(AnonymousNominalKind::Struct, "Fourth", 3),
        ];
        let ordinals = anonymous_symbol_ordinals(
            left.iter()
                .map(|site| (0xaaaa, site.clone()))
                .chain(right.iter().map(|site| (0xbbbb, site.clone()))),
        );
        assert_eq!(
            left.iter()
                .map(|site| ordinals[site])
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1, 2, 3])
        );
        assert_eq!(
            right
                .iter()
                .map(|site| ordinals[site])
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0, 1])
        );
    }

    #[test]
    fn anonymous_identity_digest_encoding_is_stable() {
        let identity = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Definition("root::make".to_string()),
            anchor: RirStructuralAnchor::new(vec![
                RirStructuralPathSegment::Body,
                RirStructuralPathSegment::AnonymousType(2),
            ]),
            arguments: CanonicalArguments {
                types: Arc::new([]),
                values: Arc::new([
                    CanonicalArgumentValue::Integer(42),
                    CanonicalArgumentValue::Bool(true),
                    CanonicalArgumentValue::String(Arc::from("rue")),
                ]),
            },
        };

        assert_eq!(
            stable_anonymous_identity_digest(&identity),
            0xdf10_a209_9a1d_9f9b_e7ee_009c_9e2d_4bfd
        );
    }

    /// The installed-endpoint component encodings, pinned byte-for-byte: the
    /// `D`/`M` formats are digest inputs shared by the epoch and the durable
    /// adapter, so any byte change here is an identity break, not a refactor.
    #[test]
    fn stable_symbol_component_encodings_are_pinned() {
        // Owner-absent definition: the owner slot renders as the empty string
        // between its two separators.
        assert_eq!(
            stable_definition_component("pkg/m.rue", "make", None, 3),
            "D\u{1}pkg/m.rue\u{1}make\u{1}\u{1}3"
        );
        // Owner-present definition (an owned method / associated definition).
        assert_eq!(
            stable_definition_component("pkg/m.rue", "get", Some("Holder"), 4),
            "D\u{1}pkg/m.rue\u{1}get\u{1}Holder\u{1}4"
        );
        // Module component.
        assert_eq!(stable_module_component("pkg/m.rue"), "M\u{1}pkg/m.rue");
    }

    /// The durable stream this module writes must be the stream
    /// `#[derive(Hash)]` used to write, or every anonymous nominal in every
    /// program silently changes name. Both are run over a corpus that reaches
    /// every variant of every type on the identity graph, so a wrong
    /// discriminant or a dropped field fails here rather than in somebody's
    /// object file.
    ///
    /// The derive is the oracle only while it still agrees; once it stops being
    /// consulted for naming, this test is what pins the encoding, and its
    /// corpus is what makes that pin worth having.
    #[test]
    fn durable_encode_agrees_with_the_derived_hash() {
        fn derived(identity: &AnonymousNominalKey<String, String>) -> u128 {
            let mut hasher = StableFnv1a128::new();
            identity.hash(&mut hasher);
            hasher.digest()
        }

        for (label, identity) in identity_corpus() {
            assert_eq!(
                stable_anonymous_identity_digest(&identity),
                derived(&identity),
                "durable encoding diverged from the derived hash for {label}"
            );
        }
    }

    /// Every `TypeInstanceKey` variant, reached through a nominal's arguments.
    fn every_type_variant() -> Vec<crate::TypeInstanceKey<String, String>> {
        use crate::TypeInstanceKey as T;
        let leaf = || Arc::new(T::I32);
        vec![
            T::I8,
            T::I16,
            T::I32,
            T::I64,
            T::U8,
            T::U16,
            T::U32,
            T::U64,
            T::Bool,
            T::Unit,
            T::Never,
            T::ComptimeType,
            T::BuiltinNominal {
                kind: AnonymousNominalKind::Enum,
                name: "str".into(),
            },
            T::Nominal(crate::NominalInstanceKey::Builtin {
                kind: AnonymousNominalKind::Struct,
                name: "builtin".into(),
            }),
            T::Nominal(crate::NominalInstanceKey::Named(
                "pkg/m.rue::Named".to_owned(),
            )),
            T::Nominal(crate::NominalInstanceKey::Anonymous(inner_nominal())),
            T::Array {
                element: leaf(),
                len: 7,
            },
            T::Slice {
                element: leaf(),
                name: "slice".into(),
            },
            T::PtrConst(leaf()),
            T::PtrMut(leaf()),
            T::Module("pkg/m.rue".to_owned()),
            T::GenericParameter(3),
        ]
    }

    /// Every `FunctionInstanceKey` variant, including both producer spellings.
    fn every_function_variant() -> Vec<crate::FunctionInstanceKey<String, String>> {
        use crate::FunctionInstanceKey as F;
        vec![
            F::Definition("pkg/m.rue::f".to_owned()),
            F::Specialization {
                base: Arc::new(F::Definition("pkg/m.rue::base".to_owned())),
                arguments: crate::CanonicalArguments::default(),
            },
            F::AnonymousMember {
                owner: Arc::new(crate::TypeInstanceKey::I64),
                member: crate::AnonymousMemberKey {
                    kind: crate::AnonymousMemberKind::Destructor,
                    name: "drop".into(),
                },
            },
            F::DropGlue(Arc::new(crate::TypeInstanceKey::Bool)),
        ]
    }

    /// Every `CanonicalArgumentValue` variant.
    fn every_argument_value() -> Vec<crate::CanonicalArgumentValue<String, String>> {
        use crate::CanonicalArgumentValue as V;
        vec![
            V::Integer(-170_141_183_460_469_231_731_687_303_715_884_105_728),
            V::Bool(true),
            V::Type(Arc::new(crate::TypeInstanceKey::U16)),
            V::Function(Arc::new(crate::FunctionInstanceKey::Definition(
                "pkg/m.rue::g".to_owned(),
            ))),
            V::Unit,
            V::String("literal".into()),
        ]
    }

    /// A nested anonymous nominal, so the recursion is exercised rather than
    /// only its leaves.
    fn inner_nominal() -> AnonymousNominalKey<String, String> {
        AnonymousNominalKey {
            kind: AnonymousNominalKind::Enum,
            producer: crate::StableProducerId::Definition("pkg/inner.rue::p".to_owned()),
            anchor: anchor(&[RirStructuralPathSegment::Body]),
            arguments: crate::CanonicalArguments::default(),
        }
    }

    fn anchor(segments: &[RirStructuralPathSegment]) -> rue_rir::RirStructuralAnchor {
        rue_rir::RirStructuralAnchor::new(segments.to_vec())
    }

    /// One identity per shape worth separating, each labelled so a failure
    /// names the variant that diverged.
    fn identity_corpus() -> Vec<(String, AnonymousNominalKey<String, String>)> {
        let mut corpus = Vec::new();

        // Both producer spellings, and both nominal kinds.
        for (kind_label, kind) in [
            ("struct", AnonymousNominalKind::Struct),
            ("enum", AnonymousNominalKind::Enum),
        ] {
            for function in every_function_variant() {
                corpus.push((
                    format!("producer function {function:?} ({kind_label})"),
                    AnonymousNominalKey {
                        kind,
                        producer: crate::StableProducerId::Function(Arc::new(function)),
                        anchor: anchor(&[RirStructuralPathSegment::AnonymousType(1)]),
                        arguments: crate::CanonicalArguments::default(),
                    },
                ));
            }
            corpus.push((
                format!("producer definition ({kind_label})"),
                AnonymousNominalKey {
                    kind,
                    producer: crate::StableProducerId::Definition("pkg/m.rue::d".to_owned()),
                    anchor: anchor(&[RirStructuralPathSegment::ReturnType]),
                    arguments: crate::CanonicalArguments::default(),
                },
            ));
        }

        // Every type variant, as the sole type argument.
        for ty in every_type_variant() {
            corpus.push((
                format!("type argument {ty:?}"),
                AnonymousNominalKey {
                    kind: AnonymousNominalKind::Struct,
                    producer: crate::StableProducerId::Definition("pkg/m.rue::d".to_owned()),
                    anchor: anchor(&[RirStructuralPathSegment::FieldType(2)]),
                    arguments: crate::CanonicalArguments {
                        types: vec![ty].into(),
                        values: Arc::new([]),
                    },
                },
            ));
        }

        // Every value variant, as the sole value argument.
        for value in every_argument_value() {
            corpus.push((
                format!("value argument {value:?}"),
                AnonymousNominalKey {
                    kind: AnonymousNominalKind::Struct,
                    producer: crate::StableProducerId::Definition("pkg/m.rue::d".to_owned()),
                    anchor: anchor(&[RirStructuralPathSegment::Method(0)]),
                    arguments: crate::CanonicalArguments {
                        types: Arc::new([]),
                        values: vec![value].into(),
                    },
                },
            ));
        }

        // Multi-element argument streams, so the length prefixes are covered
        // rather than only the zero- and one-element cases.
        corpus.push((
            "multiple arguments of both streams".to_owned(),
            AnonymousNominalKey {
                kind: AnonymousNominalKind::Enum,
                producer: crate::StableProducerId::Definition("pkg/m.rue::d".to_owned()),
                anchor: anchor(&[
                    RirStructuralPathSegment::Statement(4),
                    RirStructuralPathSegment::VariantPayload {
                        variant: 1,
                        payload: 2,
                    },
                    RirStructuralPathSegment::Operand(9),
                ]),
                arguments: crate::CanonicalArguments {
                    types: every_type_variant().into(),
                    values: every_argument_value().into(),
                },
            },
        ));

        corpus
    }
}
