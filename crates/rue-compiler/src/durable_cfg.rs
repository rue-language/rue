use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use lasso::{Key, Spur};
use rue_air::{AirInstData, AnalyzedFunction, Type, TypeKind};
use rue_span::Span;

use crate::{DurableAirInstData, DurableOrdinaryBodyPayload, DurableType};

fn deduplicate_type_mappings(
    mappings: Vec<(Type, DurableType)>,
) -> Result<Vec<(Type, DurableType)>, CfgDomainFailure> {
    let mut positions: HashMap<Type, usize> = HashMap::with_capacity(mappings.len());
    let mut unique: Vec<(Type, DurableType)> = Vec::with_capacity(mappings.len());
    for (current, stable) in mappings {
        if let Some(&position) = positions.get(&current) {
            if unique[position].1 != stable {
                return Err(CfgDomainFailure::Shape);
            }
        } else {
            positions.insert(current, unique.len());
            unique.push((current, stable));
        }
    }
    Ok(unique)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StableCfgInput {
    pub identity: Arc<str>,
    pub body_span: Span,
    pub body: DurableOrdinaryBodyPayload,
    pub specialization: Option<crate::DurableSpecializationIdentity>,
    pub type_inputs: Arc<[crate::StableDefinitionInputFingerprint]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutDependencyFailure {
    MissingDefinition,
    AmbiguousDefinition,
    MissingNominal,
    AmbiguousNominal,
}

pub(crate) fn body_type_dependencies(
    body: &DurableOrdinaryBodyPayload,
) -> BTreeSet<crate::StableDefinitionKey> {
    fn visit(value: &DurableType, keys: &mut BTreeSet<crate::StableDefinitionKey>) {
        match value {
            DurableType::Nominal(key) => {
                keys.insert(key.clone());
            }
            DurableType::Array { element, .. } => visit(element, keys),
            _ => {}
        }
    }
    let mut keys = BTreeSet::new();
    visit(&body.return_type, &mut keys);
    for instruction in body.instructions.iter() {
        visit(&instruction.ty, &mut keys);
    }
    for place in body.places.iter() {
        visit(&place.base_type, &mut keys);
        for projection in place.projections.iter() {
            match projection {
                crate::DurableProjection::Field { struct_key, .. } => {
                    keys.insert(struct_key.clone());
                }
                crate::DurableProjection::Index { array_type, .. } => visit(array_type, &mut keys),
            }
        }
    }
    for (_, ty) in body.param_drops.iter() {
        visit(ty, &mut keys);
    }
    for instruction in body.instructions.iter() {
        match &instruction.data {
            DurableAirInstData::TypeConst(ty) | DurableAirInstData::IntCast { from_ty: ty, .. } => {
                visit(ty, &mut keys)
            }
            DurableAirInstData::StructInit { struct_key, .. } => {
                keys.insert(struct_key.clone());
            }
            DurableAirInstData::EnumVariant { enum_key, .. }
            | DurableAirInstData::EnumPayloadGet { enum_key, .. } => {
                keys.insert(enum_key.clone());
            }
            _ => {}
        }
        if let DurableAirInstData::Intrinsic { name, args, .. } = &instruction.data
            && matches!(name.as_ref(), "ptr_read" | "ptr_write" | "ptr_offset")
        {
            for arg in args.iter() {
                if let Some(argument) = body.instructions.get(arg.value as usize) {
                    match &argument.ty {
                        DurableType::PtrConst(pointee) | DurableType::PtrMut(pointee) => {
                            visit(pointee, &mut keys)
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    keys
}

pub(crate) fn transitive_body_type_dependencies(
    body: &DurableOrdinaryBodyPayload,
    pool: &rue_air::FrozenTypeInternPool,
    definitions: &crate::BoundDefinitionSet,
) -> Result<BTreeSet<crate::StableDefinitionKey>, LayoutDependencyFailure> {
    let mut keys = body_type_dependencies(body);
    let mut pending = keys.iter().cloned().collect::<Vec<_>>();
    let stable_key = |file: rue_span::FileId, name: &str, kind| {
        let matches = definitions
            .definitions()
            .iter()
            .filter(|record| {
                record.stable_key().kind() == kind
                    && record.stable_key().name() == name
                    && record.declaration_span().file_id == file
            })
            .take(2)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [record] => Ok(record.stable_key().clone()),
            [] => Err(LayoutDependencyFailure::MissingDefinition),
            _ => Err(LayoutDependencyFailure::AmbiguousDefinition),
        }
    };
    while let Some(key) = pending.pop() {
        let record = definitions
            .definition_by_key(&key)
            .ok_or(LayoutDependencyFailure::MissingDefinition)?;
        let file = record.declaration_span().file_id;
        let mut nested = Vec::new();
        match key.kind() {
            crate::StableDefinitionKind::Struct => {
                let ids = pool
                    .all_struct_ids()
                    .into_iter()
                    .filter(|id| {
                        let def = pool.struct_def(*id);
                        def.file_id == file && def.name == key.name()
                    })
                    .take(2)
                    .collect::<Vec<_>>();
                let [id] = ids.as_slice() else {
                    return Err(if ids.is_empty() {
                        LayoutDependencyFailure::MissingNominal
                    } else {
                        LayoutDependencyFailure::AmbiguousNominal
                    });
                };
                nested.extend(pool.struct_def(*id).fields.iter().map(|field| field.ty));
            }
            crate::StableDefinitionKind::Enum => {
                let ids = pool
                    .all_enum_ids()
                    .into_iter()
                    .filter(|id| {
                        let def = pool.enum_def(*id);
                        def.file_id == file && def.name == key.name()
                    })
                    .take(2)
                    .collect::<Vec<_>>();
                let [id] = ids.as_slice() else {
                    return Err(if ids.is_empty() {
                        LayoutDependencyFailure::MissingNominal
                    } else {
                        LayoutDependencyFailure::AmbiguousNominal
                    });
                };
                let def = pool.enum_def(*id);
                for index in 0..def.variant_count() {
                    nested.extend_from_slice(def.variant_payload(index));
                }
            }
            _ => {}
        }
        while let Some(ty) = nested.pop() {
            let found = match ty.kind() {
                TypeKind::Struct(id) => {
                    let def = pool.struct_def(id);
                    (!def.is_builtin)
                        .then(|| {
                            stable_key(def.file_id, &def.name, crate::StableDefinitionKind::Struct)
                        })
                        .transpose()?
                }
                TypeKind::Enum(id) => {
                    let def = pool.enum_def(id);
                    Some(stable_key(
                        def.file_id,
                        &def.name,
                        crate::StableDefinitionKind::Enum,
                    )?)
                }
                TypeKind::Array(id) => {
                    nested.push(pool.array_def(id).0);
                    None
                }
                TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => None,
                _ => None,
            };
            if let Some(found) = found.filter(|found| keys.insert(found.clone())) {
                pending.push(found);
            }
        }
    }
    Ok(keys)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum StableCfgSymbol {
    Callable(crate::StableDefinitionKey),
    Specialization(crate::DurableSpecializationIdentity),
    Intrinsic(Arc<str>),
}

#[derive(Debug, Clone)]
pub(crate) struct CfgDomainProjection {
    types: Vec<(Type, DurableType)>,
    strings: Vec<(u32, Arc<str>)>,
    spans: Vec<(Span, crate::DurableBodyAnchor)>,
    symbols: Vec<(Spur, StableCfgSymbol)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CfgDomainFailure {
    Shape,
    Unsupported,
    Missing,
}

impl CfgDomainProjection {
    pub fn validate_cfg(&self, cfg: &rue_cfg::Cfg, span: Span) -> Result<(), CfgDomainFailure> {
        Self::import_cfg(self, self, cfg, span).map(|_| ())
    }
    pub fn from_body(
        function: &AnalyzedFunction,
        input: &StableCfgInput,
        strings: &[String],
    ) -> Result<Self, CfgDomainFailure> {
        if function.air.instructions().len() != input.body.instructions.len() {
            return Err(CfgDomainFailure::Shape);
        }
        let mut types = vec![(function.air.return_type(), input.body.return_type.clone())];
        let mut stable_strings = Vec::new();
        let mut spans = Vec::new();
        let mut symbols = Vec::new();
        for ((_, current), durable) in function.air.iter().zip(input.body.instructions.iter()) {
            types.push((current.ty, durable.ty.clone()));
            if current.span.file_id != input.body_span.file_id
                || current.span.start < input.body_span.start
                || current.span.end > input.body_span.end
            {
                return Err(CfgDomainFailure::Unsupported);
            }
            spans.push((
                current.span,
                crate::DurableBodyAnchor {
                    start: current.span.start - input.body_span.start,
                    end: current.span.end - input.body_span.start,
                },
            ));
            match (&current.data, &durable.data) {
                (AirInstData::StringConst(old), DurableAirInstData::StringConst(local)) => {
                    let value = input
                        .body
                        .strings
                        .get(*local as usize)
                        .ok_or(CfgDomainFailure::Shape)?;
                    if strings.get(*old as usize).map(String::as_str) != Some(value) {
                        return Err(CfgDomainFailure::Shape);
                    }
                    stable_strings.push((*old, value.clone()));
                }
                (
                    AirInstData::IntCast { from_ty, .. },
                    DurableAirInstData::IntCast {
                        from_ty: stable, ..
                    },
                ) => types.push((*from_ty, stable.clone())),
                (AirInstData::Call { name, .. }, DurableAirInstData::Call { function, .. }) => {
                    symbols.push((*name, StableCfgSymbol::Callable(function.clone())))
                }
                (
                    AirInstData::Call { name, .. },
                    DurableAirInstData::CallSpecialized { identity, .. },
                ) => symbols.push((*name, StableCfgSymbol::Specialization(identity.clone()))),
                (
                    AirInstData::Intrinsic { name, .. },
                    DurableAirInstData::Intrinsic { name: stable, .. },
                ) => symbols.push((*name, StableCfgSymbol::Intrinsic(stable.clone()))),
                _ => {}
            }
        }
        for (current, stable) in function.air.places().iter().zip(input.body.places.iter()) {
            types.push((current.base_type, stable.base_type.clone()));
            let current_projections = function.air.get_place_projections(current);
            if current_projections.len() != stable.projections.len() {
                return Err(CfgDomainFailure::Shape);
            }
            for (current, stable) in current_projections.iter().zip(stable.projections.iter()) {
                if let (
                    rue_air::AirProjection::Index { array_type, .. },
                    crate::DurableProjection::Index {
                        array_type: stable, ..
                    },
                ) = (current, stable)
                {
                    types.push((*array_type, stable.clone()));
                }
            }
        }
        let types = deduplicate_type_mappings(types)?;
        stable_strings.sort_by_key(|(index, _)| *index);
        stable_strings.dedup();
        spans.sort_by_key(|(span, _)| (span.file_id.index(), span.start, span.end));
        spans.dedup();
        symbols.sort_by(|left, right| {
            (left.0.into_usize(), &left.1).cmp(&(right.0.into_usize(), &right.1))
        });
        symbols.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        if symbols.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(CfgDomainFailure::Shape);
        }
        Ok(Self {
            types,
            strings: stable_strings,
            spans,
            symbols,
        })
    }

    fn stable_type(&self, value: Type) -> Result<DurableType, CfgDomainFailure> {
        self.types
            .iter()
            .find(|(current, _)| *current == value)
            .map(|(_, stable)| stable.clone())
            .ok_or(CfgDomainFailure::Missing)
    }
    fn current_type(&self, value: &DurableType) -> Result<Type, CfgDomainFailure> {
        self.types
            .iter()
            .find(|(_, stable)| stable == value)
            .map(|(current, _)| *current)
            .ok_or(CfgDomainFailure::Missing)
    }
    fn stable_nominal(&self, value: Type) -> Result<DurableType, CfgDomainFailure> {
        self.stable_type(value)
    }
    fn current_nominal(&self, value: &DurableType) -> Result<Type, CfgDomainFailure> {
        self.current_type(value)
    }

    pub fn import_cfg(
        old: &Self,
        new: &Self,
        cfg: &rue_cfg::Cfg,
        new_span: Span,
    ) -> Result<rue_cfg::Cfg, CfgDomainFailure> {
        cfg.try_remap_domains(
            |value| new.current_type(&old.stable_type(value)?),
            |value| match new
                .current_nominal(&old.stable_nominal(Type::new_struct(value))?)?
                .kind()
            {
                TypeKind::Struct(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value| match new
                .current_nominal(&old.stable_nominal(Type::new_enum(value))?)?
                .kind()
            {
                TypeKind::Enum(id) => Ok(id),
                _ => Err(CfgDomainFailure::Shape),
            },
            |value: Spur| {
                let stable = old
                    .symbols
                    .iter()
                    .find(|(symbol, _)| *symbol == value)
                    .map(|(_, value)| value)
                    .ok_or(CfgDomainFailure::Missing)?;
                new.symbols
                    .iter()
                    .find(|(_, identity)| identity == stable)
                    .map(|(symbol, _)| *symbol)
                    .ok_or(CfgDomainFailure::Missing)
            },
            |value| {
                let stable = old
                    .strings
                    .iter()
                    .find(|(index, _)| *index == value)
                    .map(|(_, value)| value)
                    .ok_or(CfgDomainFailure::Missing)?;
                new.strings
                    .iter()
                    .find(|(_, value)| value == stable)
                    .map(|(index, _)| *index)
                    .ok_or(CfgDomainFailure::Missing)
            },
            |value| {
                let anchor = old
                    .spans
                    .iter()
                    .find(|(span, _)| *span == value)
                    .map(|(_, anchor)| anchor)
                    .ok_or(CfgDomainFailure::Missing)?;
                Ok(Span {
                    file_id: new_span.file_id,
                    start: new_span.start + anchor.start,
                    end: new_span.start + anchor.end,
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::Key;
    use rue_cfg::{Cfg, CfgInst, CfgInstData};

    fn projection(symbol: Spur) -> CfgDomainProjection {
        CfgDomainProjection {
            types: vec![(Type::I32, DurableType::I32)],
            strings: Vec::new(),
            spans: vec![(
                Span::new(4, 5),
                crate::DurableBodyAnchor { start: 0, end: 1 },
            )],
            symbols: vec![(symbol, StableCfgSymbol::Intrinsic(Arc::from("stable")))],
        }
    }

    #[test]
    fn type_mapping_deduplication_preserves_encounter_order_and_rejects_conflicts() {
        let mappings = deduplicate_type_mappings(vec![
            (Type::I64, DurableType::I64),
            (Type::I32, DurableType::I32),
            (Type::I64, DurableType::I64),
        ])
        .unwrap();
        assert_eq!(
            mappings,
            vec![(Type::I64, DurableType::I64), (Type::I32, DurableType::I32)]
        );

        assert_eq!(
            deduplicate_type_mappings(vec![
                (Type::I32, DurableType::I32),
                (Type::I32, DurableType::I64),
            ]),
            Err(CfgDomainFailure::Shape)
        );
    }

    #[test]
    fn symbol_import_joins_to_existing_current_spur_and_failure_is_atomic() {
        let old = Spur::try_from_usize(3).unwrap();
        let new = Spur::try_from_usize(17).unwrap();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        cfg.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: old,
                    args_start: 0,
                    args_len: 0,
                },
                ty: Type::I32,
                span: Span::new(4, 5),
            },
        );
        let imported = CfgDomainProjection::import_cfg(
            &projection(old),
            &projection(new),
            &cfg,
            Span::new(40, 50),
        )
        .unwrap();
        assert!(
            matches!(imported.get_inst(rue_cfg::CfgValue::from_raw(0)).data, CfgInstData::Call { name, .. } if name == new)
        );

        let mut missing = projection(new);
        missing.symbols.clear();
        assert!(matches!(
            CfgDomainProjection::import_cfg(&projection(old), &missing, &cfg, Span::new(40, 50)),
            Err(CfgDomainFailure::Missing)
        ));
        assert!(
            matches!(cfg.get_inst(rue_cfg::CfgValue::from_raw(0)).data, CfgInstData::Call { name, .. } if name == old)
        );
    }
}
