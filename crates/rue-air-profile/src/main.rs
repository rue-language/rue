//! Fresh-process AIR timing, allocation, and side-table profiler.

use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::fs;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use lasso::{Spur, ThreadedRodeo};
use rue_air::{
    AIR_PAYLOAD_FAMILY_NAMES, Air, AirArgMode, AirCallArg, AirEditor, AirInstData, AirPattern,
    AirPayloadStorageStats, AirPlaceBase, AirProjection, ConstValue, EnumDef, EnumId, StructDef,
    StructField, StructId, Type, TypeInternPool,
};
use rue_cfg::{CFG_PAYLOAD_FAMILY_NAMES, CfgPayloadStorageStats};
use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};
use rue_rir::RIR_PAYLOAD_FAMILY_NAMES;
use rue_span::Span;
use serde_json::{Map, Value, json};

struct CountingAllocator;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

fn air_payload_storage_stats(air: &Air) -> AirPayloadStorageStats {
    let mut stats = air.payload_store_stats();
    let mut account = |family: usize, words: usize| {
        stats.family_logical_bytes[family] += words * std::mem::size_of::<u32>();
    };
    for inst in air.instructions() {
        match &inst.data {
            AirInstData::Match { arms, .. } => {
                account(0, arms.len());
                stats.nonempty_match_envelopes += usize::from(!arms.is_empty());
            }
            AirInstData::Call { args, .. } => account(1, args.len()),
            AirInstData::CallGeneric {
                type_args,
                value_args,
                args,
                ..
            } => {
                account(2, type_args.len());
                account(3, value_args.len());
                stats.peak_staging_bytes = stats
                    .peak_staging_bytes
                    .max(value_args.len() * std::mem::size_of::<u32>());
                account(1, args.len());
            }
            AirInstData::Intrinsic { args, .. } => account(4, args.len()),
            AirInstData::Block { statements, .. } => account(5, statements.len()),
            AirInstData::StructInit {
                fields,
                source_order,
                ..
            } => {
                account(6, fields.len());
                account(7, source_order.len());
            }
            AirInstData::ArrayInit { elements } => account(8, elements.len()),
            AirInstData::EnumVariant { payload, .. } => account(9, payload.len()),
            _ => {}
        }
    }
    stats.peak_staging_bytes = stats.peak_staging_bytes.max(
        air.places()
            .iter()
            .map(|place| std::mem::size_of_val(air.get_place_projections(place)))
            .max()
            .unwrap_or(0),
    );
    stats
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        pointer
    }
}

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: rue-air-profile FILE");
        process::exit(2);
    });
    if path == "--microbench" {
        println!("{}", profile_families());
        return;
    }
    let source = fs::read_to_string(&path).unwrap_or_else(|error| {
        eprintln!("cannot read {path}: {error}");
        process::exit(2);
    });
    let snapshot = SourceSnapshot::single(&path, source).unwrap_or_else(|error| {
        eprintln!("cannot create source snapshot: {error}");
        process::exit(2);
    });
    let mut session = CompilerSession::new();
    session
        .update(&snapshot)
        .into_result()
        .unwrap_or_else(|errors| {
            eprintln!("cannot publish source snapshot: {errors:?}");
            process::exit(1);
        });

    ALLOCATIONS.store(0, Ordering::SeqCst);
    ALLOCATED_BYTES.store(0, Ordering::SeqCst);
    ENABLED.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let semantic = session
        .semantic(&CompileOptions::default())
        .unwrap_or_else(|errors| {
            ENABLED.store(false, Ordering::SeqCst);
            eprintln!("semantic analysis failed: {errors:?}");
            process::exit(1);
        });
    let elapsed_ns = started.elapsed().as_nanos();
    ENABLED.store(false, Ordering::SeqCst);

    drop(session);
    let semantic = rue_compiler::unstable::into_oracle_semantic_state(semantic)
        .expect("one-shot profiler session uniquely owns its frontend artifacts");
    let rir_stats = semantic.rir_payload_storage_stats;
    let rir_family_logical_bytes: Map<String, Value> = RIR_PAYLOAD_FAMILY_NAMES
        .into_iter()
        .zip(rir_stats.family_logical_bytes)
        .map(|(name, bytes)| (name.to_owned(), json!(bytes)))
        .collect();

    let mut total = AirPayloadStorageStats::default();
    let mut cfg_total = CfgPayloadStorageStats::default();
    for function in &semantic.functions {
        let stats = air_payload_storage_stats(&function.analyzed.air);
        for (to, from) in total
            .family_logical_bytes
            .iter_mut()
            .zip(stats.family_logical_bytes)
        {
            *to += from;
        }
        total.word_store_logical_bytes += stats.word_store_logical_bytes;
        total.word_store_capacity_bytes += stats.word_store_capacity_bytes;
        total.projection_store_logical_bytes += stats.projection_store_logical_bytes;
        total.projection_store_capacity_bytes += stats.projection_store_capacity_bytes;
        total.place_store_logical_bytes += stats.place_store_logical_bytes;
        total.place_store_capacity_bytes += stats.place_store_capacity_bytes;
        total.nonempty_match_envelopes += stats.nonempty_match_envelopes;
        total.peak_staging_bytes = total.peak_staging_bytes.max(stats.peak_staging_bytes);
        let stats = function.cfg.payload_storage_stats();
        for (to, from) in cfg_total
            .family_logical_bytes
            .iter_mut()
            .zip(stats.family_logical_bytes)
        {
            *to += from;
        }
        cfg_total.value_store_logical_bytes += stats.value_store_logical_bytes;
        cfg_total.value_store_capacity_bytes += stats.value_store_capacity_bytes;
        cfg_total.call_store_logical_bytes += stats.call_store_logical_bytes;
        cfg_total.call_store_capacity_bytes += stats.call_store_capacity_bytes;
        cfg_total.switch_store_logical_bytes += stats.switch_store_logical_bytes;
        cfg_total.switch_store_capacity_bytes += stats.switch_store_capacity_bytes;
        cfg_total.projection_store_logical_bytes += stats.projection_store_logical_bytes;
        cfg_total.projection_store_capacity_bytes += stats.projection_store_capacity_bytes;
        cfg_total.nonempty_variable_envelopes += stats.nonempty_variable_envelopes;
        cfg_total.peak_staging_bytes = cfg_total.peak_staging_bytes.max(stats.peak_staging_bytes);
    }
    let family_logical_bytes: Map<String, Value> = AIR_PAYLOAD_FAMILY_NAMES
        .into_iter()
        .zip(total.family_logical_bytes)
        .map(|(name, bytes)| (name.to_owned(), json!(bytes)))
        .collect();
    let cfg_family_logical_bytes: Map<String, Value> = CFG_PAYLOAD_FAMILY_NAMES
        .into_iter()
        .zip(cfg_total.family_logical_bytes)
        .map(|(name, bytes)| (name.to_owned(), json!(bytes)))
        .collect();
    println!(
        "{}",
        json!({
            "schema_version": 2,
            "air_phase_ns": elapsed_ns,
            "allocations": ALLOCATIONS.load(Ordering::SeqCst),
            "allocated_bytes": ALLOCATED_BYTES.load(Ordering::SeqCst),
            "functions": semantic.functions.len(),
            "family_logical_bytes": family_logical_bytes,
            "word_store_logical_bytes": total.word_store_logical_bytes,
            "word_store_capacity_bytes": total.word_store_capacity_bytes,
            "projection_store_logical_bytes": total.projection_store_logical_bytes,
            "projection_store_capacity_bytes": total.projection_store_capacity_bytes,
            "place_store_logical_bytes": total.place_store_logical_bytes,
            "place_store_capacity_bytes": total.place_store_capacity_bytes,
            "total_side_table_logical_bytes": total.word_store_logical_bytes
                + total.projection_store_logical_bytes
                + total.place_store_logical_bytes,
            "total_side_table_capacity_bytes": total.word_store_capacity_bytes
                + total.projection_store_capacity_bytes
                + total.place_store_capacity_bytes,
            "nonempty_match_envelopes": total.nonempty_match_envelopes,
            "peak_staging_bytes": total.peak_staging_bytes,
            "rir": {
                "family_logical_bytes": rir_family_logical_bytes,
                "word_store_logical_bytes": rir_stats.word_store_logical_bytes,
                "word_store_capacity_bytes": rir_stats.word_store_capacity_bytes,
                "nonempty_variable_envelopes": rir_stats.nonempty_variable_envelopes,
                "peak_staging_bytes": rir_stats.peak_staging_bytes,
            },
            "cfg": {
                "family_logical_bytes": cfg_family_logical_bytes,
                "value_store_logical_bytes": cfg_total.value_store_logical_bytes,
                "value_store_capacity_bytes": cfg_total.value_store_capacity_bytes,
                "call_store_logical_bytes": cfg_total.call_store_logical_bytes,
                "call_store_capacity_bytes": cfg_total.call_store_capacity_bytes,
                "switch_store_logical_bytes": cfg_total.switch_store_logical_bytes,
                "switch_store_capacity_bytes": cfg_total.switch_store_capacity_bytes,
                "projection_store_logical_bytes": cfg_total.projection_store_logical_bytes,
                "projection_store_capacity_bytes": cfg_total.projection_store_capacity_bytes,
                "nonempty_variable_envelopes": cfg_total.nonempty_variable_envelopes,
                "peak_staging_bytes": cfg_total.peak_staging_bytes,
            },
        })
    );
}

#[derive(Clone, Copy)]
enum Family {
    Match,
    Call,
    TypeArgs,
    ConstValues,
    Intrinsic,
    Block,
    StructFields,
    SourceOrder,
    Array,
    EnumPayload,
    Projections,
}

const FAMILIES: [(&str, Family); 11] = [
    ("match_arms", Family::Match),
    ("call_args", Family::Call),
    ("type_args", Family::TypeArgs),
    ("const_values", Family::ConstValues),
    ("intrinsic_args", Family::Intrinsic),
    ("block_statements", Family::Block),
    ("struct_fields", Family::StructFields),
    ("source_order", Family::SourceOrder),
    ("array_elements", Family::Array),
    ("enum_payload", Family::EnumPayload),
    ("projections_and_places", Family::Projections),
];

struct Fixture {
    editor: AirEditor,
    values: Vec<rue_air::AirRef>,
}

fn fixture() -> Fixture {
    let mut editor = AirEditor::new(Type::UNIT);
    let values = (0..64)
        .map(|value| editor.add_const(value, Type::I32, Span::new(0, 0)))
        .collect();
    Fixture { editor, values }
}

fn build(
    fixture: &mut Fixture,
    family: Family,
    symbol: Spur,
    struct_id: StructId,
    enum_id: EnumId,
    array_type: Type,
    projection_array_types: &[Type],
) -> usize {
    let span = Span::new(0, 0);
    let values = &fixture.values;
    match family {
        Family::Match => fixture
            .editor
            .add_match(
                values[0],
                &values
                    .iter()
                    .enumerate()
                    .map(|(index, &body)| (AirPattern::Int(index as i64), body))
                    .collect::<Vec<_>>(),
                Type::I32,
                span,
            )
            .unwrap()
            .as_u32() as usize,
        Family::Call => fixture
            .editor
            .add_call(
                None,
                symbol,
                &values
                    .iter()
                    .map(|&value| AirCallArg {
                        value,
                        mode: AirArgMode::Normal,
                    })
                    .collect::<Vec<_>>(),
                Type::I32,
                span,
            )
            .unwrap()
            .as_u32() as usize,
        Family::TypeArgs => fixture
            .editor
            .add_call_generic(symbol, &[Type::I32; 64], &[], &[], Type::I32, span)
            .unwrap()
            .as_u32() as usize,
        Family::ConstValues => fixture
            .editor
            .add_call_generic(
                symbol,
                &[],
                &vec![ConstValue::Integer(7); 64],
                &[],
                Type::I32,
                span,
            )
            .unwrap()
            .as_u32() as usize,
        Family::Intrinsic => fixture
            .editor
            .add_intrinsic(None, symbol, values, Type::I32, span)
            .unwrap()
            .as_u32() as usize,
        Family::Block => fixture
            .editor
            .add_block(values, values[0], Type::I32, span)
            .unwrap()
            .as_u32() as usize,
        Family::StructFields | Family::SourceOrder => fixture
            .editor
            .add_struct_init(
                struct_id,
                values,
                &(0..64).collect::<Vec<_>>(),
                Type::new_struct(struct_id),
                span,
            )
            .unwrap()
            .as_u32() as usize,
        Family::Array => fixture
            .editor
            .add_array_init(values, array_type, span)
            .unwrap()
            .as_u32() as usize,
        Family::EnumPayload => fixture
            .editor
            .add_enum_variant(enum_id, 0, values, Type::new_enum(enum_id), span)
            .unwrap()
            .as_u32() as usize,
        Family::Projections => fixture
            .editor
            .make_place(
                AirPlaceBase::Local(0),
                projection_array_types[0],
                projection_array_types
                    .iter()
                    .map(|&array_type| AirProjection::Index {
                        array_type,
                        index: values[0],
                    }),
            )
            .unwrap()
            .as_u32() as usize,
    }
}

fn consume(fixture: &Fixture, family: Family, handle: usize) -> usize {
    if matches!(family, Family::Projections) {
        let place = fixture
            .editor
            .get_place(rue_air::AirPlaceRef::from_raw(handle as u32));
        return consume_iter(fixture.editor.get_place_projections(place).iter());
    }
    let inst = &fixture
        .editor
        .get(rue_air::AirRef::from_raw(handle as u32))
        .data;
    match (family, inst) {
        (Family::Match, AirInstData::Match { arms, .. }) => {
            consume_iter(fixture.editor.get_match_arms(arms))
        }
        (Family::Call, AirInstData::Call { args, .. }) => {
            consume_iter(fixture.editor.get_call_args(args))
        }
        (Family::TypeArgs, AirInstData::CallGeneric { type_args, .. }) => {
            consume_iter(fixture.editor.get_type_args(type_args))
        }
        (Family::ConstValues, AirInstData::CallGeneric { value_args, .. }) => {
            consume_iter(fixture.editor.get_const_values(value_args))
        }
        (Family::Intrinsic, AirInstData::Intrinsic { args, .. }) => {
            consume_iter(fixture.editor.get_intrinsic_args(args))
        }
        (Family::Block, AirInstData::Block { statements, .. }) => {
            consume_iter(fixture.editor.get_block_statements(statements))
        }
        (Family::StructFields, AirInstData::StructInit { fields, .. }) => {
            consume_iter(fixture.editor.get_struct_fields(fields))
        }
        (Family::SourceOrder, AirInstData::StructInit { source_order, .. }) => {
            consume_iter(fixture.editor.get_source_order(source_order))
        }
        (Family::Array, AirInstData::ArrayInit { elements }) => {
            consume_iter(fixture.editor.get_array_elements(elements))
        }
        (Family::EnumPayload, AirInstData::EnumVariant { payload, .. }) => {
            consume_iter(fixture.editor.get_enum_payload(payload))
        }
        _ => unreachable!(),
    }
}

fn consume_iter(iter: impl IntoIterator) -> usize {
    iter.into_iter()
        .map(|value| {
            std::hint::black_box(value);
            1
        })
        .sum()
}

fn profile_families() -> Value {
    const BUILDS: usize = 128;
    const TRAVERSALS: usize = 20_000;
    let symbols = ThreadedRodeo::new();
    let symbol = symbols.get_or_intern("profile_fn");
    let pool = TypeInternPool::new();
    let (struct_id, _) = pool.register_struct(
        symbols.get_or_intern("ProfileStruct"),
        StructDef {
            name: "ProfileStruct".into(),
            fields: (0..64)
                .map(|index| StructField {
                    name: format!("field_{index}"),
                    ty: Type::I32,
                })
                .collect(),
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        },
    );
    let (enum_id, _) = pool.register_enum(
        symbols.get_or_intern("ProfileEnum"),
        EnumDef {
            name: "ProfileEnum".into(),
            variants: std::sync::Arc::from(["Only".into()]),
            variant_payloads: vec![vec![Type::I32; 64]],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        },
    );
    let array_type = Type::new_array(pool.intern_array_from_type(Type::I32, 64));
    let mut projection_array_types = Vec::with_capacity(64);
    let mut projection_element = Type::I32;
    for _ in 0..64 {
        projection_element = Type::new_array(pool.intern_array_from_type(projection_element, 1));
        projection_array_types.push(projection_element);
    }
    projection_array_types.reverse();
    let pool = pool.freeze();

    // Prove that every synthetic owner is publishable. This validation is a
    // correctness fixture and deliberately sits outside all timed intervals.
    for (_, family) in FAMILIES {
        let mut fixture = fixture();
        build(
            &mut fixture,
            family,
            symbol,
            struct_id,
            enum_id,
            array_type,
            &projection_array_types,
        );
        fixture
            .editor
            .finish(rue_air::AirValidationContext::CanonicalWithSymbols(
                &pool, &symbols,
            ))
            .expect("microbenchmark fixture must be valid AIR");
    }
    let mut output = Map::new();
    for (family_index, (name, family)) in FAMILIES.into_iter().enumerate() {
        let mut fixtures: Vec<_> = (0..BUILDS).map(|_| fixture()).collect();
        ALLOCATIONS.store(0, Ordering::SeqCst);
        ALLOCATED_BYTES.store(0, Ordering::SeqCst);
        let mut handles = Vec::with_capacity(BUILDS);
        ENABLED.store(true, Ordering::SeqCst);
        let started = Instant::now();
        for fixture in &mut fixtures {
            handles.push(build(
                fixture,
                family,
                symbol,
                struct_id,
                enum_id,
                array_type,
                &projection_array_types,
            ));
        }
        let build_ns = started.elapsed().as_nanos();
        ENABLED.store(false, Ordering::SeqCst);
        let build_allocations = ALLOCATIONS.load(Ordering::SeqCst);
        let build_bytes = ALLOCATED_BYTES.load(Ordering::SeqCst);

        ALLOCATIONS.store(0, Ordering::SeqCst);
        ENABLED.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let mut consumed = 0usize;
        for _ in 0..TRAVERSALS {
            consumed += std::hint::black_box(consume(&fixtures[0], family, handles[0]));
        }
        let traversal_ns = started.elapsed().as_nanos();
        ENABLED.store(false, Ordering::SeqCst);
        let stats = air_payload_storage_stats(&fixtures[0].editor);
        let elements = consumed / TRAVERSALS;
        let logical_bytes = if family_index < AIR_PAYLOAD_FAMILY_NAMES.len() {
            stats.family_logical_bytes[family_index]
        } else {
            stats.projection_store_logical_bytes + stats.place_store_logical_bytes
        };
        let capacity_bytes = stats.word_store_capacity_bytes
            + stats.projection_store_capacity_bytes
            + stats.place_store_capacity_bytes;
        output.insert(
            name.into(),
            json!({
                "elements": elements,
                "builds": BUILDS,
                "build_ns": build_ns,
                "build_allocations": build_allocations,
                "build_allocated_bytes": build_bytes,
                "build_elements_per_second": (elements * BUILDS) as f64 / (build_ns as f64 / 1e9),
                "traversals": TRAVERSALS,
                "traversal_ns": traversal_ns,
                "traversal_allocations": ALLOCATIONS.load(Ordering::SeqCst),
                "elements_per_second": consumed as f64 / (traversal_ns as f64 / 1e9),
                "logical_bytes": logical_bytes,
                "capacity_bytes": capacity_bytes,
                "total_bytes": logical_bytes + capacity_bytes,
                "envelopes": if matches!(family, Family::Match) { stats.nonempty_match_envelopes } else { 0 },
                "side_table_logical_bytes": stats.word_store_logical_bytes
                    + stats.projection_store_logical_bytes + stats.place_store_logical_bytes,
                "side_table_capacity_bytes": stats.word_store_capacity_bytes
                    + stats.projection_store_capacity_bytes + stats.place_store_capacity_bytes,
                "peak_staging_bytes": stats.peak_staging_bytes,
            }),
        );
    }
    json!({"schema_version": 1, "families": output})
}
