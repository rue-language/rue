//! The one AIR authority for compiler-injected builtin nominals.
//!
//! Builtin enums and the core `str` view are synthetic declarations, but they
//! still live in the ordinary [`TypeInternPool`].  Keeping their construction
//! here makes allocation order, provenance, and lookup semantics explicit for
//! every semantic consumer.

use std::collections::BTreeMap;
use std::sync::Arc;

use lasso::{LassoErrorKind, Spur, ThreadedRodeo};
use rue_rir::SharedSymbolSpace;
use rue_span::FileId;

use crate::{
    EnumDef, EnumId, SemanticImportNominalKind, StructDef, StructField, StructId, Type,
    TypeInternPool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinNominalId {
    Enum(EnumId),
    Struct(StructId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreStrRegistrationError {
    WrongSymbol,
}

/// A staged builtin bootstrap.  Enum registration is deliberately separate
/// from core `str`: semantic import epochs insert source nominal shells between
/// those phases, and their stable pool IDs are part of the import contract.
#[derive(Debug)]
pub(crate) struct BuiltinUniverse {
    lookup: BTreeMap<(Arc<str>, SemanticImportNominalKind), BuiltinNominalId>,
}

impl BuiltinUniverse {
    pub(crate) const CORE_STR_NAME: &'static str = "str";

    /// Register the deterministic target-enum prefix of a builtin universe.
    pub(crate) fn begin(
        type_pool: &TypeInternPool,
        symbol_space: &SharedSymbolSpace,
    ) -> Result<Self, LassoErrorKind> {
        let mut lookup = BTreeMap::new();
        for builtin in rue_builtins::BUILTIN_ENUMS {
            let symbol = symbol_space.try_intern(builtin.name)?;
            let (id, _) = type_pool.register_enum(
                symbol,
                EnumDef {
                    name: Arc::from(builtin.name),
                    variants: builtin.variants.iter().map(|v| Arc::from(*v)).collect(),
                    variant_payloads: Vec::new(),
                    is_pub: true,
                    is_non_exhaustive: false,
                    file_id: FileId::DEFAULT,
                },
            );
            lookup.insert(
                (Arc::from(builtin.name), SemanticImportNominalKind::Enum),
                BuiltinNominalId::Enum(id),
            );
        }
        Ok(Self { lookup })
    }

    /// Register the core `str` identity after source nominals, when required
    /// by the importing epoch. Body identity pools call it immediately after
    /// [`Self::begin`].
    pub(crate) fn finish_core_str(
        &mut self,
        type_pool: &TypeInternPool,
        symbol_space: &SharedSymbolSpace,
    ) -> Result<StructId, LassoErrorKind> {
        let symbol = symbol_space.try_intern(Self::CORE_STR_NAME)?;
        let id = Self::register_core_str_with_symbol(type_pool, symbol_space.interner(), symbol)
            .expect("the canonical core-str symbol must resolve in its owning space");
        self.lookup.insert(
            (
                Arc::from(Self::CORE_STR_NAME),
                SemanticImportNominalKind::Struct,
            ),
            BuiltinNominalId::Struct(id),
        );
        Ok(id)
    }

    /// The ordinary body path already owns/interns its symbol and may only
    /// materialize `str`; it must not bootstrap the whole builtin universe.
    pub(crate) fn register_core_str_with_symbol(
        type_pool: &TypeInternPool,
        symbols: &ThreadedRodeo,
        symbol: Spur,
    ) -> Result<StructId, CoreStrRegistrationError> {
        if symbols.resolve(&symbol) != Self::CORE_STR_NAME {
            return Err(CoreStrRegistrationError::WrongSymbol);
        }
        let ptr_id = type_pool.intern_ptr_const_from_type(Type::U8);
        let (id, _) = type_pool.register_struct(
            symbol,
            StructDef {
                name: Arc::from(Self::CORE_STR_NAME),
                fields: vec![
                    StructField {
                        name: "ptr".to_owned(),
                        ty: Type::new_ptr_const(ptr_id),
                    },
                    StructField {
                        name: "len".to_owned(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: true,
                file_id: FileId::DEFAULT,
            },
        );
        Ok(id)
    }

    #[cfg(test)]
    pub(crate) fn lookup(
        &self,
        name: &str,
        kind: SemanticImportNominalKind,
    ) -> Option<BuiltinNominalId> {
        self.lookup.get(&(Arc::from(name), kind)).copied()
    }

    pub(crate) fn enum_entries(&self) -> impl Iterator<Item = (&Arc<str>, EnumId)> {
        self.lookup
            .iter()
            .filter(|((_, kind), _)| *kind == SemanticImportNominalKind::Enum)
            .map(|((name, _), nominal)| {
                let BuiltinNominalId::Enum(id) = *nominal else {
                    unreachable!("builtin enum lookup kind invariant")
                };
                (name, id)
            })
    }

    /// Return the canonical core `str` lookup entry after the staged bootstrap
    /// has finished. Consumers use this entry rather than rebuilding its
    /// spelling/kind pair independently.
    pub(crate) fn core_str_entry(&self) -> Option<(Arc<str>, StructId)> {
        self.lookup.iter().find_map(|((name, kind), nominal)| {
            (*kind == SemanticImportNominalKind::Struct && name.as_ref() == Self::CORE_STR_NAME)
                .then(|| {
                    let BuiltinNominalId::Struct(id) = *nominal else {
                        unreachable!("core str lookup kind invariant")
                    };
                    (Arc::clone(name), id)
                })
        })
    }

    pub(crate) fn builtin_enum_name(name: &str) -> bool {
        rue_builtins::BUILTIN_ENUMS
            .iter()
            .any(|builtin| builtin.name == name)
    }

    #[cfg(test)]
    pub(crate) const fn bootstrap_symbol_count() -> usize {
        rue_builtins::BUILTIN_ENUMS.len() + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_exact_and_staged() {
        let space = SharedSymbolSpace::private();
        let pool = TypeInternPool::new();
        let mut universe = BuiltinUniverse::begin(&pool, &space).unwrap();
        assert_eq!(BuiltinUniverse::bootstrap_symbol_count(), 4);
        assert_eq!(
            universe.lookup("Arch", SemanticImportNominalKind::Enum),
            Some(BuiltinNominalId::Enum(EnumId(0)))
        );
        assert_eq!(
            universe.lookup("Os", SemanticImportNominalKind::Enum),
            Some(BuiltinNominalId::Enum(EnumId(1)))
        );
        assert_eq!(
            universe.lookup("DataModel", SemanticImportNominalKind::Enum),
            Some(BuiltinNominalId::Enum(EnumId(2)))
        );
        let str_id = universe.finish_core_str(&pool, &space).unwrap();
        assert_eq!(str_id, StructId(4));
        assert_eq!(
            universe.lookup("str", SemanticImportNominalKind::Struct),
            Some(BuiltinNominalId::Struct(str_id))
        );
        assert_eq!(
            universe.lookup("str", SemanticImportNominalKind::Enum),
            None
        );
        assert_eq!(
            universe.lookup("Arch", SemanticImportNominalKind::Struct),
            None
        );
        let def = pool.struct_def(str_id);
        assert_eq!(def.name.as_ref(), "str");
        assert_eq!(
            def.fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["ptr", "len"]
        );
        assert!(def.is_copy && def.is_builtin && def.is_pub);
        assert!(!def.is_linear && !def.declared_linear && def.destructor.is_none());
        assert_eq!(def.file_id, FileId::DEFAULT);
        assert_eq!(pool.struct_lang_item(str_id), None);
        assert_eq!(
            def.fields[0].ty,
            Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::U8))
        );
        assert_eq!(def.fields[1].ty, Type::U64);
        for (name, variants) in [
            ("Arch", ["X86_64", "Aarch64"].as_slice()),
            ("Os", ["Linux", "Macos"].as_slice()),
            ("DataModel", ["Ilp32", "Lp64", "Llp64"].as_slice()),
        ] {
            let BuiltinNominalId::Enum(id) = universe
                .lookup(name, SemanticImportNominalKind::Enum)
                .unwrap()
            else {
                unreachable!()
            };
            let def = pool.enum_def(id);
            assert_eq!(
                def.variants
                    .iter()
                    .map(|variant| variant.as_ref())
                    .collect::<Vec<_>>(),
                variants
            );
            assert!(def.variant_payloads.is_empty() && def.is_pub && !def.is_non_exhaustive);
            assert_eq!(def.file_id, FileId::DEFAULT);
        }
    }

    #[test]
    fn ordinary_helper_is_idempotent_for_one_pool() {
        let space = SharedSymbolSpace::private();
        let pool = TypeInternPool::new();
        let symbol = space.try_intern(BuiltinUniverse::CORE_STR_NAME).unwrap();
        let first = BuiltinUniverse::register_core_str_with_symbol(&pool, space.interner(), symbol)
            .unwrap();
        let second =
            BuiltinUniverse::register_core_str_with_symbol(&pool, space.interner(), symbol)
                .unwrap();
        assert_eq!(first, second);

        let wrong = space.try_intern("not-str").unwrap();
        assert_eq!(
            BuiltinUniverse::register_core_str_with_symbol(&pool, space.interner(), wrong),
            Err(CoreStrRegistrationError::WrongSymbol)
        );
    }

    #[test]
    fn bootstrap_projects_interner_failures_without_partial_success() {
        let pool = TypeInternPool::new();
        let space = SharedSymbolSpace::with_owner_bound(0);
        assert!(matches!(
            BuiltinUniverse::begin(&pool, &space),
            Err(LassoErrorKind::KeySpaceExhaustion)
        ));

        let pool = TypeInternPool::new();
        let space = SharedSymbolSpace::with_owner_bound(3);
        let mut universe = BuiltinUniverse::begin(&pool, &space).unwrap();
        assert_eq!(
            universe.finish_core_str(&pool, &space),
            Err(LassoErrorKind::KeySpaceExhaustion)
        );
    }
}
