//! Request-independent semantic values imported into a fresh AIR epoch.
//!
//! This module deliberately stops short of installing declarations into
//! [`crate::Sema`]. Bodies, source spans, and RIR declaration handles belong to
//! an exact semantic request. The importer reconstructs only values that AIR
//! can represent without borrowing handles from the exporting request.

use std::collections::BTreeMap;
use std::hash::Hash;
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_span::FileId;

use crate::{
    ConstValue, EnumDef, EnumId, ModuleId, ModuleRegistry, StructDef, StructField, StructId, Type,
    TypeInternPool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportNominalKind {
    Struct,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticImportNominal<K> {
    pub key: K,
    pub module_path: Arc<str>,
    pub name: Arc<str>,
    pub kind: SemanticImportNominalKind,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportType<K, M> {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    Nominal(K),
    Array {
        element: Box<Self>,
        len: u64,
    },
    PtrConst(Box<Self>),
    PtrMut(Box<Self>),
    Module(M),
    GenericParameter(u32),
    Tuple(Arc<[Self]>),
    Function {
        parameters: Arc<[Self]>,
        result: Box<Self>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportConstValue<K, M> {
    Integer(i128),
    Bool(bool),
    Type(SemanticImportType<K, M>),
    Function(K),
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticImportFailure {
    DuplicateNominal,
    DuplicateNominalLocalIdentity,
    DuplicateFunction,
    DuplicateFunctionLocalIdentity,
    DeclarationKindMismatch,
    NominalKindMismatch,
    MissingNominal,
    MissingModule,
    MissingFunction,
    GenericParameterNeedsDeclarationContext,
    UnsupportedTuple,
    UnsupportedFunctionType,
    ForeignLocalType,
    ForeignLocalValue,
}

/// An AIR type branded with the import epoch which issued it.
#[derive(Debug, Clone)]
pub struct SemanticImportedType {
    epoch: Arc<()>,
    value: Type,
}

impl PartialEq for SemanticImportedType {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.value == other.value
    }
}
impl Eq for SemanticImportedType {}

/// An AIR constant branded with the import epoch which issued it.
#[derive(Debug, Clone)]
pub struct SemanticImportedConstValue {
    epoch: Arc<()>,
    value: ConstValue,
}

impl PartialEq for SemanticImportedConstValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &other.epoch) && self.value == other.value
    }
}
impl Eq for SemanticImportedConstValue {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalNominal {
    Struct(StructId),
    Enum(EnumId),
}

/// A fresh AIR-owned interning epoch joined to caller-owned stable keys.
///
/// Stable keys never become AIR IDs. They are retained only in the maps at
/// this boundary. Imported types and constants are returned through opaque,
/// epoch-branded wrappers, so request-local IDs cannot cross epoch boundaries.
pub struct SemanticImportEpoch<K: Ord, M: Ord> {
    epoch: Arc<()>,
    interner: ThreadedRodeo,
    type_pool: TypeInternPool,
    module_registry: ModuleRegistry,
    nominals: BTreeMap<K, LocalNominal>,
    functions: BTreeMap<K, Spur>,
    modules: BTreeMap<M, ModuleId>,
}

impl<K, M> SemanticImportEpoch<K, M>
where
    K: Clone + Ord,
    M: Clone + Ord + AsRef<str>,
{
    pub fn new(
        mut nominals: Vec<SemanticImportNominal<K>>,
        mut function_keys: Vec<(K, Arc<str>)>,
        mut module_keys: Vec<M>,
    ) -> Result<Self, SemanticImportFailure> {
        nominals.sort_by(|a, b| a.key.cmp(&b.key));
        function_keys.sort_by(|a, b| a.0.cmp(&b.0));
        module_keys.sort();

        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        let module_registry = ModuleRegistry::new();
        let mut modules = BTreeMap::new();
        for key in module_keys {
            let path = key.as_ref().to_owned();
            let (id, _) = module_registry.get_or_create(path.clone(), path);
            modules.insert(key, id);
        }

        let mut functions = BTreeMap::new();
        let mut function_identities = std::collections::BTreeSet::new();
        for (key, name) in function_keys {
            if !function_identities.insert(name.clone()) {
                return Err(SemanticImportFailure::DuplicateFunctionLocalIdentity);
            }
            let symbol = interner.get_or_intern(name.as_ref());
            if functions.insert(key, symbol).is_some() {
                return Err(SemanticImportFailure::DuplicateFunction);
            }
        }

        let mut module_files = BTreeMap::<Arc<str>, FileId>::new();
        for nominal in &nominals {
            let next = u32::try_from(module_files.len() + 1).expect("too many semantic modules");
            module_files
                .entry(nominal.module_path.clone())
                .or_insert(FileId::new(next));
        }
        let mut local = BTreeMap::new();
        let mut local_identities = std::collections::BTreeSet::new();
        for nominal in nominals {
            if local.contains_key(&nominal.key) {
                return Err(SemanticImportFailure::DuplicateNominal);
            }
            if !local_identities.insert((
                nominal.kind,
                nominal.module_path.clone(),
                nominal.name.clone(),
            )) {
                return Err(SemanticImportFailure::DuplicateNominalLocalIdentity);
            }
            let file_id = module_files[&nominal.module_path];
            let name = nominal.name.to_string();
            let symbol = interner.get_or_intern(&name);
            let value = match nominal.kind {
                SemanticImportNominalKind::Struct => {
                    let (id, _) = type_pool.register_struct(
                        symbol,
                        StructDef {
                            name,
                            fields: vec![],
                            is_copy: false,
                            is_linear: false,
                            destructor: None,
                            is_builtin: false,
                            is_pub: nominal.is_public,
                            file_id,
                        },
                    );
                    LocalNominal::Struct(id)
                }
                SemanticImportNominalKind::Enum => {
                    let (id, _) = type_pool.register_enum(
                        symbol,
                        EnumDef {
                            name,
                            variants: vec![],
                            variant_payloads: vec![],
                            is_pub: nominal.is_public,
                            file_id,
                        },
                    );
                    LocalNominal::Enum(id)
                }
            };
            local.insert(nominal.key, value);
        }
        Ok(Self {
            epoch: Arc::new(()),
            interner,
            type_pool,
            module_registry,
            nominals: local,
            functions,
            modules,
        })
    }

    pub fn import_type(
        &self,
        value: &SemanticImportType<K, M>,
    ) -> Result<SemanticImportedType, SemanticImportFailure> {
        self.import_type_local(value)
            .map(|value| SemanticImportedType {
                epoch: self.epoch.clone(),
                value,
            })
    }

    fn import_type_local(
        &self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, SemanticImportFailure> {
        Ok(match value {
            SemanticImportType::I8 => Type::I8,
            SemanticImportType::I16 => Type::I16,
            SemanticImportType::I32 => Type::I32,
            SemanticImportType::I64 => Type::I64,
            SemanticImportType::U8 => Type::U8,
            SemanticImportType::U16 => Type::U16,
            SemanticImportType::U32 => Type::U32,
            SemanticImportType::U64 => Type::U64,
            SemanticImportType::Bool => Type::BOOL,
            SemanticImportType::Unit => Type::UNIT,
            SemanticImportType::Never => Type::NEVER,
            SemanticImportType::ComptimeType => Type::COMPTIME_TYPE,
            SemanticImportType::Nominal(key) => match self.nominals.get(key) {
                Some(LocalNominal::Struct(id)) => Type::new_struct(*id),
                Some(LocalNominal::Enum(id)) => Type::new_enum(*id),
                None => return Err(SemanticImportFailure::MissingNominal),
            },
            SemanticImportType::Array { element, len } => Type::new_array(
                self.type_pool
                    .intern_array_from_type(self.import_type_local(element)?, *len),
            ),
            SemanticImportType::PtrConst(value) => Type::new_ptr_const(
                self.type_pool
                    .intern_ptr_const_from_type(self.import_type_local(value)?),
            ),
            SemanticImportType::PtrMut(value) => Type::new_ptr_mut(
                self.type_pool
                    .intern_ptr_mut_from_type(self.import_type_local(value)?),
            ),
            SemanticImportType::Module(key) => Type::new_module(
                *self
                    .modules
                    .get(key)
                    .ok_or(SemanticImportFailure::MissingModule)?,
            ),
            SemanticImportType::GenericParameter(_) => {
                return Err(SemanticImportFailure::GenericParameterNeedsDeclarationContext);
            }
            SemanticImportType::Tuple(_) => return Err(SemanticImportFailure::UnsupportedTuple),
            SemanticImportType::Function { .. } => {
                return Err(SemanticImportFailure::UnsupportedFunctionType);
            }
        })
    }

    /// Import an ordered declaration parameter or payload sequence.
    pub fn import_types(
        &self,
        values: &[SemanticImportType<K, M>],
    ) -> Result<Arc<[SemanticImportedType]>, SemanticImportFailure> {
        values
            .iter()
            .map(|value| self.import_type(value))
            .collect::<Result<Vec<_>, _>>()
            .map(Arc::from)
    }

    pub fn import_const_value(
        &self,
        value: &SemanticImportConstValue<K, M>,
    ) -> Result<SemanticImportedConstValue, SemanticImportFailure> {
        self.import_const_value_local(value)
            .map(|value| SemanticImportedConstValue {
                epoch: self.epoch.clone(),
                value,
            })
    }

    fn import_const_value_local(
        &self,
        value: &SemanticImportConstValue<K, M>,
    ) -> Result<ConstValue, SemanticImportFailure> {
        Ok(match value {
            SemanticImportConstValue::Integer(v) => ConstValue::Integer(*v),
            SemanticImportConstValue::Bool(v) => ConstValue::Bool(*v),
            SemanticImportConstValue::Type(v) => ConstValue::Type(self.import_type_local(v)?),
            SemanticImportConstValue::Function(key) => ConstValue::Function(
                *self
                    .functions
                    .get(key)
                    .ok_or(SemanticImportFailure::MissingFunction)?,
            ),
            SemanticImportConstValue::Unit => ConstValue::Unit,
        })
    }

    /// Project an imported local type back through this epoch's stable join.
    /// This rejects local values which were not issued by this epoch.
    pub fn export_type(
        &self,
        value: SemanticImportedType,
    ) -> Result<SemanticImportType<K, M>, SemanticImportFailure> {
        if !Arc::ptr_eq(&value.epoch, &self.epoch) {
            return Err(SemanticImportFailure::ForeignLocalType);
        }
        self.export_type_local(value.value)
    }

    fn export_type_local(
        &self,
        value: Type,
    ) -> Result<SemanticImportType<K, M>, SemanticImportFailure> {
        Ok(match value.kind() {
            crate::TypeKind::I8 => SemanticImportType::I8,
            crate::TypeKind::I16 => SemanticImportType::I16,
            crate::TypeKind::I32 => SemanticImportType::I32,
            crate::TypeKind::I64 => SemanticImportType::I64,
            crate::TypeKind::U8 => SemanticImportType::U8,
            crate::TypeKind::U16 => SemanticImportType::U16,
            crate::TypeKind::U32 => SemanticImportType::U32,
            crate::TypeKind::U64 => SemanticImportType::U64,
            crate::TypeKind::Bool => SemanticImportType::Bool,
            crate::TypeKind::Unit => SemanticImportType::Unit,
            crate::TypeKind::Never => SemanticImportType::Never,
            crate::TypeKind::ComptimeType => SemanticImportType::ComptimeType,
            crate::TypeKind::Struct(id) => SemanticImportType::Nominal(
                self.nominals
                    .iter()
                    .find_map(|(key, local)| {
                        (*local == LocalNominal::Struct(id)).then(|| key.clone())
                    })
                    .ok_or(SemanticImportFailure::ForeignLocalType)?,
            ),
            crate::TypeKind::Enum(id) => SemanticImportType::Nominal(
                self.nominals
                    .iter()
                    .find_map(|(key, local)| {
                        (*local == LocalNominal::Enum(id)).then(|| key.clone())
                    })
                    .ok_or(SemanticImportFailure::ForeignLocalType)?,
            ),
            crate::TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                SemanticImportType::Array {
                    element: Box::new(self.export_type_local(element)?),
                    len,
                }
            }
            crate::TypeKind::PtrConst(id) => SemanticImportType::PtrConst(Box::new(
                self.export_type_local(self.type_pool.ptr_const_def(id))?,
            )),
            crate::TypeKind::PtrMut(id) => SemanticImportType::PtrMut(Box::new(
                self.export_type_local(self.type_pool.ptr_mut_def(id))?,
            )),
            crate::TypeKind::Module(id) => SemanticImportType::Module(
                self.modules
                    .iter()
                    .find_map(|(key, local)| (*local == id).then(|| key.clone()))
                    .ok_or(SemanticImportFailure::ForeignLocalType)?,
            ),
            crate::TypeKind::Error => return Err(SemanticImportFailure::ForeignLocalType),
        })
    }

    pub fn export_const_value(
        &self,
        value: SemanticImportedConstValue,
    ) -> Result<SemanticImportConstValue<K, M>, SemanticImportFailure> {
        if !Arc::ptr_eq(&value.epoch, &self.epoch) {
            return Err(SemanticImportFailure::ForeignLocalValue);
        }
        Ok(match value.value {
            ConstValue::Integer(v) => SemanticImportConstValue::Integer(v),
            ConstValue::Bool(v) => SemanticImportConstValue::Bool(v),
            ConstValue::Type(v) => SemanticImportConstValue::Type(self.export_type_local(v)?),
            ConstValue::Function(symbol) => SemanticImportConstValue::Function(
                self.functions
                    .iter()
                    .find_map(|(key, local)| (*local == symbol).then(|| key.clone()))
                    .ok_or(SemanticImportFailure::ForeignLocalValue)?,
            ),
            ConstValue::Unit => SemanticImportConstValue::Unit,
        })
    }

    pub fn complete_struct(
        &self,
        key: &K,
        fields: &[(Arc<str>, SemanticImportType<K, M>)],
        is_copy: bool,
        is_linear: bool,
    ) -> Result<(), SemanticImportFailure> {
        let LocalNominal::Struct(id) = self
            .nominals
            .get(key)
            .copied()
            .ok_or(SemanticImportFailure::MissingNominal)?
        else {
            return Err(SemanticImportFailure::NominalKindMismatch);
        };
        let mut def = self.type_pool.struct_def(id);
        def.fields = fields
            .iter()
            .map(|(name, ty)| {
                Ok(StructField {
                    name: name.to_string(),
                    ty: self.import_type_local(ty)?,
                })
            })
            .collect::<Result<_, _>>()?;
        def.is_copy = is_copy;
        def.is_linear = is_linear;
        self.type_pool.update_struct_def(id, def);
        Ok(())
    }

    pub fn complete_enum(
        &self,
        key: &K,
        variants: &[(Arc<str>, Arc<[SemanticImportType<K, M>]>)],
    ) -> Result<(), SemanticImportFailure> {
        let LocalNominal::Enum(id) = self
            .nominals
            .get(key)
            .copied()
            .ok_or(SemanticImportFailure::MissingNominal)?
        else {
            return Err(SemanticImportFailure::NominalKindMismatch);
        };
        let mut def = self.type_pool.enum_def(id);
        def.variants = variants.iter().map(|(name, _)| name.to_string()).collect();
        def.variant_payloads = variants
            .iter()
            .map(|(_, payload)| {
                payload
                    .iter()
                    .map(|ty| self.import_type_local(ty))
                    .collect()
            })
            .collect::<Result<_, _>>()?;
        self.type_pool.update_enum_def(id, def);
        Ok(())
    }

    pub fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }
    pub fn interner(&self) -> &ThreadedRodeo {
        &self.interner
    }
    pub fn module_registry(&self) -> &ModuleRegistry {
        &self.module_registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypeKind;

    type Epoch = SemanticImportEpoch<&'static str, &'static str>;
    type ImportType = SemanticImportType<&'static str, &'static str>;

    fn nominal(
        key: &'static str,
        name: &'static str,
        kind: SemanticImportNominalKind,
    ) -> SemanticImportNominal<&'static str> {
        SemanticImportNominal {
            key,
            module_path: Arc::from("pkg/main.rue"),
            name: Arc::from(name),
            kind,
            is_public: true,
        }
    }

    fn projection(epoch: &Epoch, ty: Type) -> String {
        match ty.kind() {
            TypeKind::I32 => "i32".into(),
            TypeKind::Bool => "bool".into(),
            TypeKind::Struct(id) => format!("struct {}", epoch.type_pool().struct_def(id).name),
            TypeKind::Enum(id) => format!("enum {}", epoch.type_pool().enum_def(id).name),
            TypeKind::PtrConst(id) => format!(
                "ptr const {}",
                projection(epoch, epoch.type_pool().ptr_const_def(id))
            ),
            TypeKind::PtrMut(id) => format!(
                "ptr mut {}",
                projection(epoch, epoch.type_pool().ptr_mut_def(id))
            ),
            TypeKind::Array(id) => {
                let (element, len) = epoch.type_pool().array_def(id);
                format!("[{element}; {len}]", element = projection(epoch, element))
            }
            TypeKind::Module(id) => {
                format!("module {}", epoch.module_registry().get_def(id).file_path)
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn fresh_epochs_remap_to_equivalent_values_despite_different_local_ids() {
        let a = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let b = Epoch::new(
            vec![
                nominal("noise", "Aardvark", SemanticImportNominalKind::Enum),
                nominal("node", "Node", SemanticImportNominalKind::Struct),
            ],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let durable = ImportType::PtrConst(Box::new(ImportType::Nominal("node")));
        let ta = a.import_type(&durable).unwrap();
        let tb = b.import_type(&durable).unwrap();
        assert_ne!(
            ta.value, tb.value,
            "the test must exercise different request-local IDs"
        );
        assert_eq!(projection(&a, ta.value), projection(&b, tb.value));
        let va = a
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        let vb = b
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        assert_eq!(
            matches!(va.value, ConstValue::Function(_)),
            matches!(vb.value, ConstValue::Function(_))
        );
    }

    #[test]
    fn two_phase_shells_support_cycles_and_input_order_is_irrelevant() {
        let declarations = vec![
            nominal("right", "Right", SemanticImportNominalKind::Struct),
            nominal("left", "Left", SemanticImportNominalKind::Struct),
        ];
        let first = Epoch::new(declarations.clone(), vec![], vec!["pkg/main.rue"]).unwrap();
        let second = Epoch::new(
            declarations.into_iter().rev().collect(),
            vec![],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let left_fields = [(
            Arc::from("next"),
            ImportType::PtrConst(Box::new(ImportType::Nominal("right"))),
        )];
        let right_fields = [(
            Arc::from("next"),
            ImportType::PtrMut(Box::new(ImportType::Nominal("left"))),
        )];
        first
            .complete_struct(&"left", &left_fields, false, false)
            .unwrap();
        first
            .complete_struct(&"right", &right_fields, false, false)
            .unwrap();
        second
            .complete_struct(&"right", &right_fields, false, false)
            .unwrap();
        second
            .complete_struct(&"left", &left_fields, false, false)
            .unwrap();
        for key in ["left", "right"] {
            let ty = ImportType::Nominal(key);
            assert_eq!(
                projection(&first, first.import_type(&ty).unwrap().value),
                projection(&second, second.import_type(&ty).unwrap().value)
            );
        }
    }

    #[test]
    fn unsupported_and_foreign_values_fail_closed() {
        let epoch = Epoch::new(vec![], vec![], vec![]).unwrap();
        assert_eq!(
            epoch.import_type(&ImportType::Nominal("missing")),
            Err(SemanticImportFailure::MissingNominal)
        );
        assert_eq!(
            epoch.import_type(&ImportType::Module("missing")),
            Err(SemanticImportFailure::MissingModule)
        );
        assert_eq!(
            epoch.import_type(&ImportType::GenericParameter(0)),
            Err(SemanticImportFailure::GenericParameterNeedsDeclarationContext)
        );
        assert_eq!(
            epoch.import_type(&ImportType::Tuple(Arc::from([]))),
            Err(SemanticImportFailure::UnsupportedTuple)
        );
        assert_eq!(
            epoch.import_const_value(&SemanticImportConstValue::Function("missing")),
            Err(SemanticImportFailure::MissingFunction)
        );
    }

    #[test]
    fn supported_types_and_values_round_trip_exactly() {
        let epoch = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec!["pkg/main.rue"],
        )
        .unwrap();
        let values = [
            ImportType::Array {
                element: Box::new(ImportType::PtrConst(Box::new(ImportType::Nominal("node")))),
                len: 7,
            },
            ImportType::Module("pkg/main.rue"),
            ImportType::Bool,
        ];
        for value in values {
            assert_eq!(
                epoch
                    .export_type(epoch.import_type(&value).unwrap())
                    .unwrap(),
                value
            );
        }
        assert_eq!(
            epoch
                .import_types(&[ImportType::Bool, ImportType::I32])
                .unwrap()
                .as_ref(),
            &[
                SemanticImportedType {
                    epoch: epoch.epoch.clone(),
                    value: Type::BOOL
                },
                SemanticImportedType {
                    epoch: epoch.epoch.clone(),
                    value: Type::I32
                },
            ]
        );
        let value = SemanticImportConstValue::Function("f");
        assert_eq!(
            epoch
                .export_const_value(epoch.import_const_value(&value).unwrap())
                .unwrap(),
            value
        );
    }

    #[test]
    fn foreign_epoch_values_fail_closed_even_when_raw_ids_alias() {
        let a = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec![],
        )
        .unwrap();
        let b = Epoch::new(
            vec![nominal("node", "Node", SemanticImportNominalKind::Struct)],
            vec![("f", Arc::from("f"))],
            vec![],
        )
        .unwrap();
        let ty = a.import_type(&ImportType::Nominal("node")).unwrap();
        let value = a
            .import_const_value(&SemanticImportConstValue::Function("f"))
            .unwrap();
        assert_eq!(
            b.export_type(ty),
            Err(SemanticImportFailure::ForeignLocalType)
        );
        assert_eq!(
            b.export_const_value(value),
            Err(SemanticImportFailure::ForeignLocalValue)
        );
    }

    #[test]
    fn duplicate_stable_and_local_identities_are_rejected() {
        assert!(matches!(
            Epoch::new(
                vec![],
                vec![("f", Arc::from("a")), ("f", Arc::from("b"))],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateFunction)
        ));
        assert!(matches!(
            Epoch::new(
                vec![],
                vec![("a", Arc::from("same")), ("b", Arc::from("same"))],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateFunctionLocalIdentity)
        ));
        assert!(matches!(
            Epoch::new(
                vec![
                    nominal("a", "Node", SemanticImportNominalKind::Struct),
                    nominal("b", "Node", SemanticImportNominalKind::Struct),
                ],
                vec![],
                vec![]
            ),
            Err(SemanticImportFailure::DuplicateNominalLocalIdentity)
        ));
    }
}
