//! Bounded property and corruption coverage for ADR-0024 compact types.
//!
//! Encoding-shape properties deliberately live apart from pool-authoritative
//! validation properties: a well-shaped `Type` can still be foreign to a
//! pool, out of range, the wrong kind, incomplete, or recovery-only.

use ahash::AHashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Barrier};

use lasso::ThreadedRodeo;
use rue_span::FileId;

use crate::type_encoding::{self, Composite};
use crate::{
    ArrayTypeId, EnumDef, EnumId, ModuleId, PtrConstTypeId, PtrMutTypeId, StructDef, StructField,
    StructId, Type, TypeInternPool, TypeKind, TypeValidationError,
};

fn hash(value: Type) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn empty_struct(name: &str) -> StructDef {
    StructDef {
        name: name.into(),
        fields: vec![],
        is_copy: false,
        is_linear: false,
        declared_linear: false,
        destructor: None,
        is_builtin: false,
        is_pub: false,
        file_id: FileId::DEFAULT,
    }
}

fn empty_enum(name: &str) -> EnumDef {
    EnumDef {
        name: name.into(),
        variants: Arc::from(["Only".into()]),
        variant_payloads: vec![vec![]],
        is_pub: false,
        is_non_exhaustive: false,
        file_id: FileId::DEFAULT,
    }
}

#[test]
fn encoding_shape_is_exact_and_checked_round_trips_preserve_kind_metadata() {
    // Exact values make this fail if two primitive tags are swapped while the
    // set of accepted primitive encodings remains unchanged.
    let primitives = [
        (Type::I8, 0, TypeKind::I8),
        (Type::I16, 1, TypeKind::I16),
        (Type::I32, 2, TypeKind::I32),
        (Type::I64, 3, TypeKind::I64),
        (Type::U8, 4, TypeKind::U8),
        (Type::U16, 5, TypeKind::U16),
        (Type::U32, 6, TypeKind::U32),
        (Type::U64, 7, TypeKind::U64),
        (Type::BOOL, 8, TypeKind::Bool),
        (Type::UNIT, 9, TypeKind::Unit),
        (Type::ERROR, 10, TypeKind::Error),
        (Type::NEVER, 11, TypeKind::Never),
        (Type::COMPTIME_TYPE, 12, TypeKind::ComptimeType),
    ];
    for (ty, raw, kind) in primitives {
        assert_eq!(ty.raw_encoding(), raw);
        assert_eq!(ty.try_kind(), Some(kind));
        assert_eq!(Type::try_from_u32(raw), Some(ty));
        assert_eq!(hash(Type::try_from_u32(raw).unwrap()), hash(ty));
        assert!(!format!("{ty:?}").is_empty());
        assert!(!format!("{ty}").is_empty());
    }

    for payload in [0, 1, 0x12_3456, type_encoding::MAX_PAYLOAD] {
        let composites = [
            (
                Type::new_struct(StructId::from_pool_index(payload)),
                Composite::Struct,
                TypeKind::Struct(StructId::from_pool_index(payload)),
            ),
            (
                Type::new_enum(EnumId::from_pool_index(payload)),
                Composite::Enum,
                TypeKind::Enum(EnumId::from_pool_index(payload)),
            ),
            (
                Type::new_array(ArrayTypeId::from_pool_index(payload)),
                Composite::Array,
                TypeKind::Array(ArrayTypeId::from_pool_index(payload)),
            ),
            (
                Type::new_module(ModuleId::new(payload)),
                Composite::Module,
                TypeKind::Module(ModuleId::new(payload)),
            ),
            (
                Type::new_ptr_const(PtrConstTypeId::from_pool_index(payload)),
                Composite::PtrConst,
                TypeKind::PtrConst(PtrConstTypeId::from_pool_index(payload)),
            ),
            (
                Type::new_ptr_mut(PtrMutTypeId::from_pool_index(payload)),
                Composite::PtrMut,
                TypeKind::PtrMut(PtrMutTypeId::from_pool_index(payload)),
            ),
        ];
        let mut raw_values = AHashSet::new();
        for (ty, encoded_kind, decoded_kind) in composites {
            let expected = type_encoding::encode_composite(encoded_kind, payload).unwrap();
            assert_eq!(ty.raw_encoding(), expected);
            assert_eq!(ty.try_kind(), Some(decoded_kind));
            assert_eq!(Type::try_from_u32(expected), Some(ty));
            assert_eq!(hash(Type::try_from_u32(expected).unwrap()), hash(ty));
            assert!(
                raw_values.insert(expected),
                "composite kind metadata was lost"
            );
            assert!(!format!("{ty:?}").is_empty());
            assert!(!format!("{ty}").is_empty());
        }
    }
}

#[test]
fn encoding_shape_rejects_every_reserved_tag_and_primitive_payload() {
    let payloads = [0, 1, 0x12_3456, type_encoding::MAX_PAYLOAD];
    for tag in type_encoding::RESERVED_AFTER_PRIMITIVES_START
        ..=type_encoding::RESERVED_AFTER_PRIMITIVES_END
    {
        for payload in payloads {
            let raw = (payload << type_encoding::PAYLOAD_SHIFT) | tag;
            assert!(!Type::is_valid_encoding(raw), "reserved tag {tag:#x}");
            assert_eq!(Type::try_from_u32(raw), None);
        }
    }
    for tag in type_encoding::RESERVED_AFTER_COMPOSITES_START
        ..=type_encoding::RESERVED_AFTER_COMPOSITES_END
    {
        for payload in payloads {
            let raw = (payload << type_encoding::PAYLOAD_SHIFT) | tag;
            assert!(!Type::is_valid_encoding(raw), "reserved tag {tag:#x}");
            assert_eq!(Type::try_from_u32(raw), None);
        }
    }
    for primitive_tag in 0..=15 {
        for payload in [1, 0x12_3456, type_encoding::MAX_PAYLOAD] {
            let raw = (payload << type_encoding::PAYLOAD_SHIFT) | primitive_tag;
            assert!(!Type::is_valid_encoding(raw));
            assert_eq!(Type::try_from_u32(raw), None);
        }
    }
}

#[derive(Clone, Copy)]
struct GraphType {
    ty: Type,
    contains_error: bool,
}

#[test]
fn bounded_composite_graphs_round_trip_through_canonical_pool_storage() {
    let symbols = ThreadedRodeo::default();
    let pool = TypeInternPool::new();
    let (structure, _) =
        pool.register_struct(symbols.get_or_intern("Record"), empty_struct("Record"));
    let (enumeration, _) =
        pool.register_enum(symbols.get_or_intern("Choice"), empty_enum("Choice"));
    let maximum_length_array = pool.try_intern_array(Type::U8, u64::MAX).unwrap();
    assert_eq!(
        pool.get_array_info(maximum_length_array),
        Some((Type::U8, u64::MAX))
    );

    let mut all = vec![
        GraphType {
            ty: Type::I8,
            contains_error: false,
        },
        GraphType {
            ty: Type::I16,
            contains_error: false,
        },
        GraphType {
            ty: Type::I32,
            contains_error: false,
        },
        GraphType {
            ty: Type::I64,
            contains_error: false,
        },
        GraphType {
            ty: Type::U8,
            contains_error: false,
        },
        GraphType {
            ty: Type::U16,
            contains_error: false,
        },
        GraphType {
            ty: Type::U32,
            contains_error: false,
        },
        GraphType {
            ty: Type::U64,
            contains_error: false,
        },
        GraphType {
            ty: Type::BOOL,
            contains_error: false,
        },
        GraphType {
            ty: Type::UNIT,
            contains_error: false,
        },
        GraphType {
            ty: Type::NEVER,
            contains_error: false,
        },
        GraphType {
            ty: Type::ERROR,
            contains_error: true,
        },
        GraphType {
            ty: Type::new_struct(structure),
            contains_error: false,
        },
        GraphType {
            ty: Type::new_enum(enumeration),
            contains_error: false,
        },
        GraphType {
            ty: maximum_length_array,
            contains_error: false,
        },
    ];
    let mut frontier = all.clone();

    // Every Array/PtrConst/PtrMut nesting word through depth three is present.
    for depth in 1..=3 {
        let mut next = Vec::with_capacity(frontier.len() * 3);
        for graph in frontier {
            let array = pool.try_intern_array(graph.ty, depth).unwrap();
            assert_eq!(pool.try_intern_array(graph.ty, depth), Ok(array));
            assert_eq!(pool.get_array_info(array), Some((graph.ty, depth)));
            next.push(GraphType { ty: array, ..graph });

            let ptr_const = pool.try_intern_ptr_const(graph.ty).unwrap();
            assert_eq!(pool.try_intern_ptr_const(graph.ty), Ok(ptr_const));
            assert_eq!(
                pool.ptr_const_def(ptr_const.as_ptr_const().unwrap()),
                graph.ty
            );
            next.push(GraphType {
                ty: ptr_const,
                ..graph
            });

            let ptr_mut = pool.try_intern_ptr_mut(graph.ty).unwrap();
            assert_eq!(pool.try_intern_ptr_mut(graph.ty), Ok(ptr_mut));
            assert_eq!(pool.ptr_mut_def(ptr_mut.as_ptr_mut().unwrap()), graph.ty);
            next.push(GraphType {
                ty: ptr_mut,
                ..graph
            });
        }
        all.extend(next.iter().copied());
        frontier = next;
    }

    let mut unique = AHashSet::new();
    for graph in all {
        let raw = graph.ty.raw_encoding();
        let decoded = Type::try_from_u32(raw).expect("pool-issued encoding must decode");
        assert_eq!(decoded, graph.ty);
        assert_eq!(hash(decoded), hash(graph.ty));
        assert!(
            unique.insert(graph.ty),
            "different generated shapes aliased"
        );
        assert_eq!(pool.validate_structural_child(graph.ty), Ok(()));
        assert_eq!(
            pool.validate_complete_type(graph.ty),
            if graph.contains_error {
                Err(TypeValidationError::RecoveryType)
            } else {
                Ok(())
            }
        );
        assert!(!format!("{:?}", graph.ty).is_empty());
        assert!(!graph.ty.safe_name_with_pool(Some(&pool)).is_empty());
    }

    // Non-structural live kinds remain valid roots, but cannot become children.
    for root in [Type::COMPTIME_TYPE, Type::new_module(ModuleId::new(0))] {
        assert_eq!(pool.validate_complete_type(root), Ok(()));
        assert_eq!(Type::try_from_u32(root.raw_encoding()), Some(root));
    }
    assert_eq!(
        pool.clone().freeze().validate_for_success(),
        Err(TypeValidationError::RecoveryType)
    );
}

#[test]
fn pool_validation_rejects_wrong_kinds_ranges_and_illegal_children_without_aliasing() {
    let symbols = ThreadedRodeo::default();
    let pool = TypeInternPool::new();
    let (structure, _) = pool.register_struct(symbols.get_or_intern("S"), empty_struct("S"));
    let (enumeration, _) = pool.register_enum(symbols.get_or_intern("E"), empty_enum("E"));
    let array = pool.try_intern_array(Type::I32, 7).unwrap();
    let ptr_const = pool.try_intern_ptr_const(Type::I32).unwrap();
    let ptr_mut = pool.try_intern_ptr_mut(Type::I32).unwrap();
    let entries = [
        (structure.pool_index(), 0usize),
        (enumeration.pool_index(), 1),
        (array.as_array().unwrap().pool_index(), 2),
        (ptr_const.as_ptr_const().unwrap().pool_index(), 3),
        (ptr_mut.as_ptr_mut().unwrap().pool_index(), 4),
    ];

    for (pool_index, actual_kind) in entries {
        let forged = [
            Type::new_struct(StructId::from_pool_index(pool_index)),
            Type::new_enum(EnumId::from_pool_index(pool_index)),
            Type::new_array(ArrayTypeId::from_pool_index(pool_index)),
            Type::new_ptr_const(PtrConstTypeId::from_pool_index(pool_index)),
            Type::new_ptr_mut(PtrMutTypeId::from_pool_index(pool_index)),
        ];
        assert_eq!(
            forged.into_iter().collect::<AHashSet<_>>().len(),
            forged.len()
        );
        for (encoded_kind, ty) in forged.into_iter().enumerate() {
            assert_eq!(
                pool.validate_structural_child(ty),
                if encoded_kind == actual_kind {
                    Ok(())
                } else {
                    Err(TypeValidationError::KindMismatch)
                }
            );
        }
    }

    for ty in [
        Type::new_struct(StructId::from_pool_index(type_encoding::MAX_PAYLOAD)),
        Type::new_enum(EnumId::from_pool_index(type_encoding::MAX_PAYLOAD)),
        Type::new_array(ArrayTypeId::from_pool_index(type_encoding::MAX_PAYLOAD)),
        Type::new_ptr_const(PtrConstTypeId::from_pool_index(type_encoding::MAX_PAYLOAD)),
        Type::new_ptr_mut(PtrMutTypeId::from_pool_index(type_encoding::MAX_PAYLOAD)),
    ] {
        assert!(ty.is_valid());
        assert_eq!(
            pool.validate_structural_child(ty),
            Err(TypeValidationError::PoolIndexOutOfRange)
        );
    }

    let malformed = Type::from_u32(0x100);
    for illegal in [
        Type::COMPTIME_TYPE,
        Type::new_module(ModuleId::new(0)),
        malformed,
    ] {
        let expected = match illegal.try_kind() {
            Some(TypeKind::ComptimeType) => TypeValidationError::ComptimeStructuralChild,
            Some(TypeKind::Module(_)) => TypeValidationError::ModuleStructuralChild,
            None => TypeValidationError::InvalidEncoding,
            _ => unreachable!(),
        };
        assert_eq!(pool.try_intern_array(illegal, 1), Err(expected));
        assert_eq!(pool.try_intern_ptr_const(illegal), Err(expected));
        assert_eq!(pool.try_intern_ptr_mut(illegal), Err(expected));
    }
}

#[test]
fn incomplete_lifecycles_allow_graph_construction_but_hide_layout_and_definitions() {
    let symbols = ThreadedRodeo::default();
    let pool = TypeInternPool::new();
    let node_name = symbols.get_or_intern("Node");
    let (node, inserted) = pool.declare_struct(node_name, empty_struct("Node"));
    assert!(inserted);
    let (duplicate, inserted) = pool.declare_struct(node_name, empty_struct("Node"));
    assert_eq!(duplicate, node);
    assert!(!inserted);

    let node_ty = Type::new_struct(node);
    assert!(pool.get(node_ty).is_none());
    assert!(pool.try_struct_def(node).is_none());
    assert_eq!(
        pool.try_abi_slot_count(node_ty),
        Err(TypeValidationError::IncompleteDefinition)
    );
    let pointer = pool.try_intern_ptr_const(node_ty).unwrap();
    let by_value = pool.try_intern_array(node_ty, 1).unwrap();
    assert_eq!(pool.validate_structural_child(pointer), Ok(()));
    assert_eq!(pool.validate_structural_child(by_value), Ok(()));

    // Completion may temporarily form a by-value cycle. Semantic analysis's
    // recursive-value-type check is the authority that reports E0483;
    // layout reads stay unavailable until completion.
    pool.complete_declared_struct(
        node,
        StructDef {
            fields: vec![
                StructField {
                    name: "next".into(),
                    ty: pointer,
                },
                StructField {
                    name: "inline".into(),
                    ty: by_value,
                },
            ],
            ..empty_struct("Node")
        },
    );
    assert!(pool.try_struct_def(node).is_some());
    assert_eq!(pool.validate_complete_type(node_ty), Ok(()));

    let reserved = pool.reserve_struct_id();
    let reserved_ty = Type::new_struct(reserved);
    assert!(pool.get(reserved_ty).is_none());
    assert_eq!(
        pool.validate_structural_child(reserved_ty),
        Err(TypeValidationError::ReservedEntry)
    );
    pool.complete_struct_registration(
        reserved,
        symbols.get_or_intern("Anonymous"),
        empty_struct("Anonymous"),
    );
    assert!(pool.try_struct_def(reserved).is_some());
}

#[test]
fn concurrent_reads_and_canonical_interning_converge_deterministically() {
    let pool = Arc::new(TypeInternPool::new());
    let barrier = Arc::new(Barrier::new(9));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let array = pool.try_intern_array(Type::NEVER, 4).unwrap();
            let ptr_const = pool.try_intern_ptr_const(array).unwrap();
            let ptr_mut = pool.try_intern_ptr_mut(ptr_const).unwrap();
            for _ in 0..64 {
                assert_eq!(pool.get_array_info(array), Some((Type::NEVER, 4)));
                assert_eq!(pool.validate_complete_type(ptr_mut), Ok(()));
            }
            (array, ptr_const, ptr_mut)
        }));
    }
    barrier.wait();
    let results = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(|result| *result == results[0]));
    assert_eq!(pool.len(), 3);
    assert_eq!(pool.validate_complete_type(results[0].2), Ok(()));
}
