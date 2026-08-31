//! Cross-layer regression tests for the compact RIR owner.

use super::*;

#[cfg(test)]
mod resource_limit_tests {
    use super::*;

    #[test]
    fn resource_limit_message_names_the_published_ceiling() {
        // RUE-1221 / spec C.1:2: the diagnostic must name the exceeded limit.
        let payload = RirPayloadBuildError::ResourceLimitExceeded {
            family: "payload words",
        };
        let instructions = RirPayloadBuildError::ResourceLimitExceeded {
            family: "instructions",
        };
        assert_eq!(
            payload.to_string(),
            "RIR payload words exceeded the implementation limit of 4294967295 per program \
             (spec Appendix C.6:1)"
        );
        assert!(instructions.to_string().contains("RIR instructions"));
        assert!(instructions.to_string().contains("4294967295"));
    }

    #[test]
    fn build_failures_are_classified_for_the_user() {
        assert!(RirPayloadBuildError::ResourceLimitExceeded { family: "f" }.is_resource_limit());
        assert!(
            !RirPayloadBuildError::ResourceLimitExceeded { family: "f" }.is_resource_exhaustion()
        );
        assert!(RirPayloadBuildError::CapacityFailure { family: "f" }.is_resource_exhaustion());
        assert!(!RirPayloadBuildError::CapacityFailure { family: "f" }.is_resource_limit());
        let invalid = RirPayloadBuildError::InvalidBuilderInput {
            family: "f",
            reason: "r",
        };
        assert!(!invalid.is_resource_limit());
        assert!(!invalid.is_resource_exhaustion());
    }

    #[test]
    fn instruction_capacity_latch_is_clear_for_an_ordinary_owner() {
        let mut editor = RirEditor::new();
        editor.add_inst(Inst {
            data: InstData::IntConst(7),
            span: Span::default(),
        });
        assert!(editor.capacity_error().is_none());
    }

    #[test]
    fn published_instruction_ceiling_matches_the_addressable_index_space() {
        // `InstRef` reserves `u32::MAX` as the null payload, so the last
        // addressable index is `u32::MAX - 1` and the capacity is exactly the
        // published ceiling (spec Appendix C.6:1).
        assert_eq!(MAX_RIR_ENTRIES_PER_PROGRAM, u32::MAX);
        assert_eq!(u64::from(MAX_RIR_ENTRIES_PER_PROGRAM), 4_294_967_295);
    }
}

#[cfg(test)]
mod typed_payload_tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    thread_local! {
        static SLICE_DECODE_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    fn counted_word(record: &[u32]) -> u32 {
        SLICE_DECODE_COUNT.with(|count| count.set(count.get() + 1));
        record[0]
    }

    fn reset_decode_count() {
        SLICE_DECODE_COUNT.with(|count| count.set(0));
    }

    fn decode_count() -> usize {
        SLICE_DECODE_COUNT.with(Cell::get)
    }

    struct CountingAllocator;

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
        static ALLOCATION_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                    ALLOCATION_BYTES.with(|bytes| bytes.set(bytes.get() + layout.size()));
                }
            });
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn allocations_during(f: impl FnOnce()) -> usize {
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(|count| count.set(0));
        ALLOCATION_BYTES.with(|bytes| bytes.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        f();
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(Cell::get)
    }

    fn allocation_evidence(f: impl FnOnce()) -> (usize, usize) {
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(|count| count.set(0));
        ALLOCATION_BYTES.with(|bytes| bytes.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        f();
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        (
            ALLOCATION_COUNT.with(Cell::get),
            ALLOCATION_BYTES.with(Cell::get),
        )
    }

    #[test]
    fn validated_fixed_slice_decodes_only_requested_records() {
        let words: Vec<u32> = (0..128).collect();
        reset_decode_count();
        let view = RirSlice::new_validated(&words, 1, counted_word);
        assert_eq!(decode_count(), 0);
        assert_eq!(view.get(97), Some(97));
        assert_eq!(decode_count(), 1);
        assert_eq!(view.get(13), Some(13));
        assert_eq!(decode_count(), 2);
        assert_eq!(view.get(usize::MAX), None);
        assert_eq!(view.get(view.len()), None);
        assert_eq!(decode_count(), 2);

        let wide = RirSlice::new_validated(&[], 2, counted_word);
        assert_eq!(wide.get(usize::MAX / 2), None); // checked start + width overflow
        assert_eq!(wide.get(usize::MAX), None); // checked index * width overflow
    }

    #[test]
    fn unvalidated_fixed_slice_sweeps_before_exposing_values() {
        let words: Vec<u32> = (0..128).collect();
        reset_decode_count();
        let view = RirSlice::new_unvalidated(&words, 1, counted_word);
        assert_eq!(decode_count(), words.len());
        assert_eq!(view.get(0), Some(0));
        assert_eq!(decode_count(), words.len() + 1);
    }

    #[test]
    fn validated_fixed_slice_construction_has_no_record_scaled_allocations() {
        let small = [0u32; 8];
        let large = [0u32; 1024];
        let (small_allocations, small_bytes) =
            allocation_evidence(|| drop(RirSlice::new_validated(&small, 1, counted_word)));
        let (large_allocations, large_bytes) =
            allocation_evidence(|| drop(RirSlice::new_validated(&large, 1, counted_word)));
        assert_eq!((small_allocations, small_bytes), (0, 0));
        assert_eq!((large_allocations, large_bytes), (0, 0));
    }

    fn span() -> Span {
        Span::with_file(FileId::new(7), 3, 9)
    }

    fn install_named_types(rir: &mut Rir, symbols: &[Spur]) -> Vec<RirTypeSyntaxRef> {
        let mut builder = RirTypeSyntaxBuilder::default();
        let references = symbols
            .iter()
            .map(|symbol| builder.push_named_type(*symbol).unwrap())
            .collect();
        rir.type_syntax = builder.finish();
        references
    }

    #[test]
    fn every_payload_family_round_trips() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let r0 = InstRef::from_raw(0);
        let r1 = InstRef::from_raw(1);
        let mut rir = Rir::new();
        let types = install_named_types(&mut rir, &[a, b]);
        let (type_a, type_b) = (types[0], types[1]);

        let intrinsic = rir.add_intrinsic_args(&[r0, r1]).unwrap();
        let internal = rir.add_internal_intrinsic_args(&[r1]).unwrap();
        let block = rir.add_block_insts(&[r0, r1]).unwrap();
        let methods = rir.add_struct_methods(&[r0]).unwrap();
        let anon_methods = rir.add_anon_struct_methods(&[r1]).unwrap();
        let elements = rir.add_array_elements(&[r0, r1]).unwrap();
        assert_eq!(
            rir.intrinsic_args(&intrinsic).values().collect::<Vec<_>>(),
            [r0, r1]
        );
        assert_eq!(
            rir.internal_intrinsic_args(&internal)
                .values()
                .collect::<Vec<_>>(),
            [r1]
        );
        assert_eq!(
            rir.block_insts(&block).values().collect::<Vec<_>>(),
            [r0, r1]
        );
        assert_eq!(rir.block_inst_count(&block), 2);
        assert_eq!(rir.block_inst(&block, 0), Some(r0));
        assert_eq!(rir.block_inst(&block, 1), Some(r1));
        assert_eq!(rir.block_inst(&block, 2), None);
        assert_eq!(
            rir.struct_methods(&methods).values().collect::<Vec<_>>(),
            [r0]
        );
        assert_eq!(
            rir.anon_struct_methods(&anon_methods)
                .values()
                .collect::<Vec<_>>(),
            [r1]
        );
        assert_eq!(
            rir.array_elements(&elements).values().collect::<Vec<_>>(),
            [r0, r1]
        );

        let call = rir
            .add_call_args(&[RirCallArg {
                value: r1,
                mode: RirArgMode::Inout,
            }])
            .unwrap();
        assert_eq!(rir.call_args(&call).get(0).unwrap().value, r1);
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_b,
                mode: RirParamMode::Borrow,
                is_comptime: true,
                span: span(),
            }])
            .unwrap();
        assert_eq!(rir.params(&params).get(0).unwrap().name, a);
        let arms = rir
            .add_match_arms(&[(RirPattern::Wildcard(span()), r0)])
            .unwrap();
        assert_eq!(rir.match_arms(&arms).get(0).unwrap().1, r0);
        let inits = rir.add_field_inits(&[(a, r1)]).unwrap();
        assert_eq!(rir.field_inits(&inits).get(0).unwrap(), (a, r1));
        let fields = rir.add_struct_fields(&[(a, type_b)]).unwrap();
        let anon_fields = rir.add_anon_struct_fields(&[(b, type_a)]).unwrap();
        assert_eq!(rir.struct_fields(&fields).get(0).unwrap(), (a, type_b));
        assert_eq!(
            rir.anon_struct_fields(&anon_fields).get(0).unwrap(),
            (b, type_a)
        );
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![b],
                span: span(),
            }])
            .unwrap();
        assert_eq!(rir.directives(&directives).get(0).unwrap().name, a);
        let variants = rir.add_enum_variants(&[a, b]).unwrap();
        let anon_variants = rir.add_anon_enum_variants(&[b]).unwrap();
        assert_eq!(rir.enum_variants(&variants).to_vec(), [a, b]);
        assert_eq!(rir.anon_enum_variants(&anon_variants).to_vec(), [b]);
        let payloads = rir.add_enum_payloads(&[vec![type_a], vec![]]).unwrap();
        let anon_payloads = rir.add_anon_enum_payloads(&[vec![type_b]]).unwrap();
        assert_eq!(
            rir.enum_payloads(&payloads, &variants)
                .map(|payload| payload.to_vec())
                .collect::<Vec<_>>(),
            [vec![type_a], vec![]]
        );
        assert_eq!(
            rir.anon_enum_payloads(&anon_payloads, &anon_variants)
                .map(|payload| payload.to_vec())
                .collect::<Vec<_>>(),
            [vec![type_b]]
        );
    }

    fn every_payload_family_validated_rir() -> (ValidatedRir, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let mut editor = RirEditor::new();
        let type_a = editor.add_named_type(a).unwrap();
        let type_b = editor.add_named_type(b).unwrap();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let block = editor.add_block(&[value], span()).unwrap();
        let directives = [RirDirective {
            name: a,
            args: vec![b],
            span: span(),
        }];
        let function = editor
            .add_fn_decl(
                &directives,
                true,
                false,
                false,
                false,
                a,
                &[RirParam {
                    name: a,
                    ty: type_b,
                    mode: RirParamMode::Borrow,
                    is_comptime: false,
                    span: span(),
                }],
                type_b,
                block,
                false,
                RirParamMode::Normal,
                false,
                false,
                span(),
            )
            .unwrap();
        editor
            .add_match(
                value,
                &[(
                    RirPattern::Path {
                        module: Some(value),
                        ctor_head: Some(value),
                        type_name: a,
                        variant: b,
                        bindings: vec![a],
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        let arguments = [RirCallArg {
            value,
            mode: RirArgMode::Borrow,
        }];
        editor.add_call(a, &arguments, span()).unwrap();
        editor.add_intrinsic(a, &[value], span()).unwrap();
        editor
            .add_internal_intrinsic(InternalIntrinsic::IterLen, &[value], span())
            .unwrap();
        editor
            .add_struct_decl(
                &directives,
                true,
                false,
                a,
                &[(a, type_b)],
                &[function],
                span(),
            )
            .unwrap();
        editor
            .add_struct_init(
                Some(value),
                Some(value),
                a,
                &[(a, value)],
                Some(span()),
                span(),
            )
            .unwrap();
        editor
            .add_enum_decl(true, false, a, &[a, b], &[vec![type_b], vec![]], span())
            .unwrap();
        editor.add_array_init(&[value, block], span()).unwrap();
        editor
            .add_anon_struct_type(
                &[(b, type_a)],
                &[function],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0)]),
                span(),
            )
            .unwrap();
        editor
            .add_anon_enum_type(
                &[b],
                &[vec![type_a]],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(1)]),
                span(),
            )
            .unwrap();
        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(FileId::new(7), 100)],
        };
        (ValidatedRir::finish(editor, &context).unwrap(), interner)
    }

    fn every_span_family_validated_rir(
        shorthand: bool,
        file: FileId,
        shift: u32,
    ) -> (ValidatedRir, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let at = |position| Span::with_file(file, shift + position, shift + position + 1);
        let directive = |position| RirDirective {
            name: a,
            args: vec![b],
            span: at(position),
        };

        let mut editor = RirEditor::new();
        let type_b = editor.add_named_type(b).unwrap();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: at(0),
        });
        editor
            .add_fn_decl(
                &[directive(2)],
                false,
                false,
                false,
                false,
                a,
                &[RirParam {
                    name: a,
                    ty: type_b,
                    mode: RirParamMode::Normal,
                    is_comptime: false,
                    span: at(3),
                }],
                type_b,
                value,
                false,
                RirParamMode::Normal,
                false,
                false,
                at(1),
            )
            .unwrap();
        editor
            .add_match(
                value,
                &[
                    (RirPattern::Wildcard(at(5)), value),
                    (
                        RirPattern::Int {
                            value: 1,
                            negative: false,
                            span: at(6),
                        },
                        value,
                    ),
                    (RirPattern::Bool(true, at(7)), value),
                    (
                        RirPattern::Path {
                            module: None,
                            ctor_head: None,
                            type_name: a,
                            variant: b,
                            bindings: vec![a],
                            span: at(8),
                        },
                        value,
                    ),
                ],
                at(4),
            )
            .unwrap();
        editor
            .add_const_decl(&[directive(10)], false, a, Some(type_b), value, at(9))
            .unwrap();
        editor
            .add_alloc(
                &[directive(12)],
                Some(a),
                false,
                Some(type_b),
                value,
                false,
                at(11),
            )
            .unwrap();
        editor
            .add_struct_decl(
                &[directive(14)],
                false,
                false,
                a,
                &[(a, type_b)],
                &[],
                at(13),
            )
            .unwrap();
        editor
            .add_struct_init(
                None,
                None,
                a,
                &[(a, value)],
                shorthand.then(|| at(16)),
                at(15),
            )
            .unwrap();
        editor
            .add_anon_struct_type(
                &[(a, type_b)],
                &[],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(7)]),
                at(17),
            )
            .unwrap();
        editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: at(18),
        });

        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(file, shift + 100)],
        };
        (ValidatedRir::finish(editor, &context).unwrap(), interner)
    }

    fn span_entries(rir: &ValidatedRir) -> Vec<(RirSpanSlot, Span)> {
        let mut entries = Vec::new();
        rir.try_visit_span_slots(
            || Ok::<_, std::convert::Infallible>(()),
            |slot, span| {
                entries.push((slot, span));
                Ok(())
            },
        )
        .unwrap();
        entries
    }

    #[test]
    fn canonical_span_slots_inventory_every_storage_family() {
        let (rir, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let entries = span_entries(&rir);
        assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(
            entries
                .iter()
                .filter(|(slot, _)| slot.field() == RirSpanField::Instruction)
                .count(),
            rir.len()
        );
        for expected in [
            RirSpanField::FunctionDirective { directive: 0 },
            RirSpanField::FunctionParameter { parameter: 0 },
            RirSpanField::ConstDirective { directive: 0 },
            RirSpanField::AllocDirective { directive: 0 },
            RirSpanField::StructDirective { directive: 0 },
            RirSpanField::StructInitShorthand,
        ] {
            assert_eq!(
                entries
                    .iter()
                    .filter(|(slot, _)| slot.field() == expected)
                    .count(),
                1,
                "missing span family {expected:?}"
            );
        }
        assert_eq!(
            entries
                .iter()
                .filter(|(slot, _)| matches!(slot.field(), RirSpanField::MatchPattern { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn span_slot_schema_ignores_coordinates_and_optional_slots_do_not_renumber_peers() {
        let (first, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let (relocated, _) = every_span_family_validated_rir(true, FileId::new(9), 40);
        let first_entries = span_entries(&first);
        let relocated_entries = span_entries(&relocated);
        assert_eq!(
            first_entries
                .iter()
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>(),
            relocated_entries
                .iter()
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>()
        );
        assert!(
            first_entries
                .iter()
                .zip(&relocated_entries)
                .all(|((_, left), (_, right))| left != right)
        );

        let (explicit, _) = every_span_family_validated_rir(false, FileId::new(7), 0);
        let explicit_slots = span_entries(&explicit)
            .into_iter()
            .map(|(slot, _)| slot)
            .collect::<Vec<_>>();
        let shorthand_slot = first_entries
            .iter()
            .find(|(slot, _)| slot.field() == RirSpanField::StructInitShorthand)
            .unwrap()
            .0;
        let without_optional = first_entries
            .iter()
            .map(|(slot, _)| *slot)
            .filter(|slot| *slot != shorthand_slot)
            .collect::<Vec<_>>();
        assert_eq!(explicit_slots, without_optional);
        assert!(explicit_slots.iter().any(|slot| {
            slot.instruction().as_u32() > shorthand_slot.instruction().as_u32()
                && slot.field() == RirSpanField::Instruction
        }));
    }

    #[test]
    fn slot_aware_remap_is_atomic_and_preserves_anonymous_anchors() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let source_anchor = source
            .iter()
            .find_map(|(_, inst)| match &inst.data {
                InstData::AnonStructType { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .unwrap();
        let mut destination = RirEditor::new();
        destination
            .try_append_remapped_with_span_slots(
                &source,
                std::convert::identity,
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    let tag_offset = match slot.field() {
                        RirSpanField::Instruction => 100,
                        _ => 200,
                    };
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + tag_offset,
                        span.end + tag_offset,
                    ))
                },
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 1000)],
            },
        )
        .unwrap();
        let destination_entries = span_entries(&destination);
        assert!(destination_entries.iter().all(|(slot, span)| {
            span.file_id == FileId::new(9)
                && span.start
                    >= if slot.field() == RirSpanField::Instruction {
                        100
                    } else {
                        200
                    }
        }));
        let destination_anchor = destination
            .iter()
            .find_map(|(_, inst)| match &inst.data {
                InstData::AnonStructType { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(destination_anchor, source_anchor);
    }

    #[test]
    fn validated_span_rewrite_preserves_storage_and_covers_every_span_family() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let instruction_storage = source.0.instructions.as_ptr();
        let instruction_capacity = source.0.instructions.capacity();
        let payload_storage = source.0.extra.as_ptr();
        let payload_capacity = source.0.extra.capacity();
        let source_entries = span_entries(&source);
        let mut visited = Vec::new();

        let rewritten = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 1000)],
                },
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    visited.push(slot);
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + 40,
                        span.end + 40,
                    ))
                },
            )
            .unwrap();
        let (expected, _) = every_span_family_validated_rir(true, FileId::new(9), 40);

        assert_eq!(rewritten.0.instructions.as_ptr(), instruction_storage);
        assert_eq!(rewritten.0.instructions.capacity(), instruction_capacity);
        assert_eq!(rewritten.0.extra.as_ptr(), payload_storage);
        assert_eq!(rewritten.0.extra.capacity(), payload_capacity);
        assert_eq!(
            visited,
            source_entries
                .iter()
                .map(|(slot, _)| *slot)
                .collect::<Vec<_>>()
        );
        assert!(rewritten.exact_eq(&expected));
        assert_eq!(span_entries(&rewritten), span_entries(&expected));
    }

    #[test]
    fn validated_span_rewrite_rejects_mapping_and_context_failures() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let rejected_slot = span_entries(&source)
            .into_iter()
            .find(|(slot, _)| slot.field() == RirSpanField::StructInitShorthand)
            .unwrap()
            .0;
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 1000)],
                },
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    if slot == rejected_slot {
                        Err("rejected mapping")
                    } else {
                        Ok(span)
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::Mapping {
                slot,
                error: "rejected mapping"
            } if slot == rejected_slot
        ));

        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 10)],
                },
                || Ok::<_, std::convert::Infallible>(()),
                |_slot, span| Ok(Span::with_file(FileId::new(9), span.start, span.end)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::MalformedPayload(RirPayloadError {
                reason: "span range is outside its canonical source",
                ..
            })
        ));

        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let remaining = std::cell::Cell::new(span_entries(&source).len());
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(7), 100)],
                },
                || {
                    if remaining.get() == 0 {
                        Err("target validation canceled")
                    } else {
                        Ok(())
                    }
                },
                |_slot, span| {
                    remaining.set(remaining.get() - 1);
                    Ok(span)
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::Checkpoint("target validation canceled")
        ));
    }

    #[test]
    fn validated_rir_retained_charge_counts_shared_anchor_pointees_per_path() {
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("anchor-heavy");
        let anchor = RirStructuralAnchor::new(vec![
            RirStructuralPathSegment::Body,
            RirStructuralPathSegment::Statement(1),
            RirStructuralPathSegment::Operand(2),
            RirStructuralPathSegment::StringLiteral(3),
        ]);
        let anchor_pointee = std::mem::size_of_val(anchor.segments()) as u64;
        let span = Span::with_file(FileId::new(7), 0, 1);
        let mut editor = RirEditor::new();
        let named_type = editor.add_named_type(name).unwrap();
        editor.add_inst(Inst {
            data: InstData::TypeConst {
                type_name: named_type,
            },
            span,
        });
        editor.add_inst(Inst {
            data: InstData::StringConst {
                content: name,
                anchor: anchor.clone(),
            },
            span,
        });
        editor.add_inst(Inst {
            data: InstData::VarRef {
                name,
                anchor: Some(anchor.clone()),
            },
            span,
        });
        editor
            .add_anon_struct_type(&[], &[], anchor.clone(), span)
            .unwrap();
        editor.add_anon_enum_type(&[], &[], anchor, span).unwrap();
        let rir = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 1)],
            },
        )
        .unwrap();

        let dense = (rir.len() * std::mem::size_of::<Inst>()) as u64
            + (rir.extra_len() * std::mem::size_of::<u32>()) as u64
            + rir.type_syntax().retained_allocation_charge();
        assert_eq!(
            rir.retained_allocation_charge(),
            dense + 4 * anchor_pointee,
            "each of four reaching instructions charges the shared Arc pointee in full"
        );
    }

    #[test]
    fn declaration_interval_projection_is_candidate_local_and_rejects_open_owner_edges() {
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("f");
        let mut source = RirEditor::new();
        let unit = source.add_unit_type().unwrap();
        let mut method_roots = Vec::new();
        for _ in 0..3 {
            let body = source.add_inst(Inst {
                data: InstData::UnitConst,
                span: span(),
            });
            method_roots.push(
                source
                    .add_fn_decl(
                        &[],
                        false,
                        false,
                        false,
                        false,
                        name,
                        &[],
                        unit,
                        body,
                        true,
                        RirParamMode::Normal,
                        false,
                        false,
                        span(),
                    )
                    .unwrap(),
            );
        }
        source
            .add_struct_decl(&[], false, false, name, &[], &method_roots, span())
            .unwrap();
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 100)],
            },
        )
        .unwrap();

        let mut projected = RirEditor::new();
        projected
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                2..4,
                std::convert::identity,
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| Ok(span),
            )
            .unwrap();
        assert_eq!(projected.len(), 2);
        let InstData::FnDecl { body, .. } = projected.get(InstRef::from_raw(1)).data else {
            panic!("middle method root must remain a function declaration")
        };
        assert_eq!(body, InstRef::from_raw(0));

        let before = (projected.len(), projected.extra_len());
        let error = projected
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                6..7,
                std::convert::identity,
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::ForeignInstructionEdge {
                instruction,
                child
            } if instruction == InstRef::from_raw(6) && method_roots.contains(&child)
        ));
        assert_eq!((projected.len(), projected.extra_len()), before);
    }

    #[test]
    fn methodless_struct_shell_composition_preserves_payloads_and_wires_existing_methods() {
        let source_symbols = ThreadedRodeo::new();
        let source_name = source_symbols.get_or_intern("Container");
        let source_field = source_symbols.get_or_intern("value");
        let source_ty = source_symbols.get_or_intern("i32");
        let source_directive = source_symbols.get_or_intern("derive");
        let source_arg = source_symbols.get_or_intern("copy");
        let mut source = RirEditor::new();
        let source_ty = source.add_named_type(source_ty).unwrap();
        let source_root = source
            .add_struct_decl(
                &[RirDirective {
                    name: source_directive,
                    args: vec![source_arg],
                    span: Span::with_file(FileId::new(7), 11, 17),
                }],
                true,
                true,
                source_name,
                &[(source_field, source_ty)],
                &[],
                Span::with_file(FileId::new(7), 3, 40),
            )
            .unwrap();
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: source_symbols.len(),
                source_lengths: &[(FileId::new(7), 100)],
            },
        )
        .unwrap();

        let destination_symbols = ThreadedRodeo::new();
        let method_name = destination_symbols.get_or_intern("method");
        let mut destination = RirEditor::new();
        let unit = destination.add_unit_type().unwrap();
        let mut methods = Vec::new();
        for _ in 0..2 {
            let body = destination.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::with_file(FileId::new(9), 1, 2),
            });
            methods.push(
                destination
                    .add_fn_decl(
                        &[],
                        false,
                        false,
                        false,
                        false,
                        method_name,
                        &[],
                        unit,
                        body,
                        true,
                        RirParamMode::Normal,
                        false,
                        false,
                        Span::with_file(FileId::new(9), 1, 2),
                    )
                    .unwrap(),
            );
        }
        let range = destination
            .try_append_methodless_struct_shell_with_methods(
                &source,
                source_root,
                &methods,
                |symbol| {
                    destination_symbols.get_or_intern(
                        source_symbols
                            .try_resolve(&symbol)
                            .expect("source shell symbol belongs to its interner"),
                    )
                },
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| {
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + 100,
                        span.end + 100,
                    ))
                },
            )
            .unwrap();
        assert_eq!(range.instructions, 4..5);
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: destination_symbols.len(),
                source_lengths: &[(FileId::new(9), 1000)],
            },
        )
        .unwrap();
        let InstData::StructDecl {
            directives,
            is_pub,
            is_linear,
            name,
            fields,
            methods: actual_methods,
        } = &destination.get(InstRef::from_raw(4)).data
        else {
            panic!("composed shell must remain a struct declaration")
        };
        assert!(*is_pub);
        assert!(*is_linear);
        assert_eq!(destination_symbols.resolve(name), "Container");
        assert_eq!(
            destination
                .struct_fields(fields)
                .values()
                .map(|(name, ty)| (
                    destination_symbols.resolve(&name),
                    destination
                        .type_syntax()
                        .render_type_with(ty, |symbol| destination_symbols.resolve(symbol))
                        .unwrap(),
                ))
                .collect::<Vec<_>>(),
            [("value", "i32".to_owned())]
        );
        assert_eq!(
            destination
                .struct_methods(actual_methods)
                .values()
                .collect::<Vec<_>>(),
            methods
        );
        let directives = destination
            .directives(directives)
            .iter()
            .collect::<Vec<_>>();
        assert_eq!(directives.len(), 1);
        assert_eq!(destination_symbols.resolve(&directives[0].name), "derive");
        assert_eq!(
            directives[0]
                .args
                .values()
                .map(|arg| destination_symbols.resolve(&arg))
                .collect::<Vec<_>>(),
            ["copy"]
        );
        assert_eq!(
            directives[0].span,
            Span::with_file(FileId::new(9), 111, 117)
        );
        assert_eq!(
            destination.get(InstRef::from_raw(4)).span,
            Span::with_file(FileId::new(9), 103, 140)
        );
    }

    #[test]
    fn struct_shell_composition_rejects_invalid_sources_atomically() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("S");
        let validation = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::new(7), 100)],
        };

        let mut non_struct = RirEditor::new();
        non_struct.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let non_struct = ValidatedRir::finish(non_struct, &validation).unwrap();

        let mut with_method = RirEditor::new();
        let unit = with_method.add_unit_type().unwrap();
        let body = with_method.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let method = with_method
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                name,
                &[],
                unit,
                body,
                true,
                RirParamMode::Normal,
                false,
                false,
                span(),
            )
            .unwrap();
        let nonempty_root = with_method
            .add_struct_decl(&[], false, false, name, &[], &[method], span())
            .unwrap();
        let with_method = ValidatedRir::finish(with_method, &validation).unwrap();

        let mut destination = RirEditor::new();
        let before = (destination.len(), destination.extra_len());
        for (source, root, expected_reason) in [
            (
                &non_struct,
                InstRef::from_raw(0),
                "source root is not a struct declaration",
            ),
            (
                &with_method,
                nonempty_root,
                "source struct declaration is not methodless",
            ),
        ] {
            let error = destination
                .try_append_methodless_struct_shell_with_methods(
                    source,
                    root,
                    &[],
                    std::convert::identity,
                    || Ok::<_, std::convert::Infallible>(()),
                    |_, span| Ok(span),
                )
                .unwrap_err();
            assert!(matches!(
                error,
                RirSpanRemapError::Build(RirPayloadBuildError::InvalidBuilderInput {
                    family: "struct shell composition",
                    reason,
                }) if reason == expected_reason
            ));
            assert_eq!((destination.len(), destination.extra_len()), before);
        }
    }

    #[test]
    fn declaration_interval_projection_cancellation_rolls_back_atomically() {
        let (source, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let mut destination = RirEditor::new();
        let mut checkpoints = 0_u32;
        let before = (destination.len(), destination.extra_len());
        let error = destination
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                0..u32::try_from(source.len()).unwrap(),
                std::convert::identity,
                || {
                    checkpoints += 1;
                    (checkpoints < 3).then_some(()).ok_or("canceled")
                },
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(error, RirSpanRemapError::Checkpoint("canceled")));
        assert_eq!((destination.len(), destination.extra_len()), before);
    }

    #[test]
    fn slot_aware_remap_cancellation_rolls_back_partial_append() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let mut traversal_checkpoints = 0;
        source
            .try_visit_span_slots(
                || {
                    traversal_checkpoints += 1;
                    Ok::<_, std::convert::Infallible>(())
                },
                |_, _| Ok(()),
            )
            .unwrap();

        let mut destination = RirEditor::new();
        let prefix = destination.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        destination
            .add_call(interner.get("a").unwrap(), &[], span())
            .unwrap();
        let before = (destination.len(), destination.extra_len(), prefix);
        let mut checkpoints = 0;
        let error = destination
            .try_append_remapped_with_span_slots(
                &source,
                std::convert::identity,
                || {
                    checkpoints += 1;
                    if checkpoints > traversal_checkpoints + 2 {
                        Err("cancelled")
                    } else {
                        Ok(())
                    }
                },
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(error, RirSpanRemapError::Checkpoint("cancelled")));
        assert_eq!(
            (destination.len(), destination.extra_len()),
            (before.0, before.1)
        );
    }

    #[test]
    fn raw_span_traversal_reports_malformed_payload() {
        let mut rir = Rir::new();
        rir.extra.extend([1, PatternKind::Path as u32]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = rir
            .try_visit_span_slots(|| Ok::<_, std::convert::Infallible>(()), |_, _| Ok(()))
            .unwrap_err();
        assert!(matches!(error, RirSpanTraversalError::MalformedPayload(_)));
    }

    #[test]
    fn large_span_remap_work_and_allocations_are_linear() {
        const COUNT: usize = 4096;
        let mut source = RirEditor::new();
        for index in 0..COUNT {
            source.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(index as u32, index as u32 + 1),
            });
        }
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: 0,
                source_lengths: &[(FileId::new(0), COUNT as u32 + 1)],
            },
        )
        .unwrap();
        let mut destination = RirEditor::new();
        let mut checkpoints = 0;
        let mut mappings = 0;
        let (allocations, _) = allocation_evidence(|| {
            destination
                .try_append_remapped_with_span_slots(
                    &source,
                    std::convert::identity,
                    || {
                        checkpoints += 1;
                        Ok::<_, std::convert::Infallible>(())
                    },
                    |_, span| {
                        mappings += 1;
                        Ok(span)
                    },
                )
                .unwrap();
        });
        assert_eq!(mappings, COUNT);
        assert_eq!(checkpoints, COUNT * 2);
        assert!(
            allocations < 64,
            "dense remap unexpectedly allocated {allocations} times"
        );
    }

    #[test]
    fn append_remapped_covers_every_payload_family_at_nonzero_offsets() {
        let (source, interner) = every_payload_family_validated_rir();
        let a = interner.get("a").unwrap();
        let b = interner.get("b").unwrap();
        let mut destination = RirEditor::new();
        let prefix = destination.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::with_file(FileId::new(9), 0, 1),
        });
        destination
            .add_call(
                a,
                &[RirCallArg {
                    value: prefix,
                    mode: RirArgMode::Normal,
                }],
                span(),
            )
            .unwrap();
        let instruction_offset = destination.len() as u32;
        let payload_offset = destination.extra_len() as u32;
        assert_ne!(instruction_offset, 0);
        assert_ne!(payload_offset, 0);

        let appended = destination
            .append_remapped_with_spans(
                &source,
                |symbol| if symbol == a { b } else { a },
                |source| Span::with_file(FileId::new(9), source.start + 10, source.end + 10),
            )
            .unwrap();
        assert_eq!(appended.instructions.start, instruction_offset);
        assert_eq!(appended.extra.start, payload_offset);
        assert_eq!(appended.instructions.len(), source.len());
        assert_eq!(appended.extra.len(), source.extra_len());

        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(FileId::new(7), 100), (FileId::new(9), 1000)],
        };
        let destination = ValidatedRir::finish(destination, &context).unwrap();
        assert!(
            destination
                .iter()
                .skip(instruction_offset as usize)
                .all(|(_, instruction)| instruction.span.file_id == FileId::new(9))
        );
        let appended_function = destination
            .iter()
            .skip(instruction_offset as usize)
            .find_map(|(_, instruction)| match &instruction.data {
                InstData::FnDecl {
                    directives, params, ..
                } => Some((directives, params)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            destination
                .directives(appended_function.0)
                .get(0)
                .unwrap()
                .name,
            b
        );
        assert_eq!(
            destination.params(appended_function.1).get(0).unwrap().span,
            Span::with_file(FileId::new(9), 13, 19)
        );
        let appended_match = destination
            .iter()
            .skip(instruction_offset as usize)
            .find_map(|(_, instruction)| match &instruction.data {
                InstData::Match { arms, .. } => Some(arms),
                _ => None,
            })
            .unwrap();
        let (pattern, body) = destination.match_arms(appended_match).get(0).unwrap();
        assert_eq!(body.as_u32(), instruction_offset);
        match pattern {
            RirPatternView::Path {
                type_name,
                bindings,
                span,
                ..
            } => {
                assert_eq!(type_name, b);
                assert_eq!(bindings.to_vec(), [b]);
                assert_eq!(span.file_id, FileId::new(9));
            }
            _ => panic!("expected remapped path pattern"),
        }
        let remapped_anchors = destination
            .iter()
            .skip(instruction_offset as usize)
            .filter_map(|(_, instruction)| match &instruction.data {
                InstData::AnonStructType { anchor, .. } | InstData::AnonEnumType { anchor, .. } => {
                    Some(anchor.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            remapped_anchors,
            [
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0)]),
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(1)]),
            ]
        );
    }

    #[test]
    fn append_remapped_preserves_string_anchor_across_symbol_and_file_domains() {
        let interner = ThreadedRodeo::new();
        let source_symbol = interner.get_or_intern("source");
        let destination_symbol = interner.get_or_intern("destination");
        let anchor = RirStructuralAnchor::new(vec![
            RirStructuralPathSegment::Body,
            RirStructuralPathSegment::Statement(2),
            RirStructuralPathSegment::StringLiteral(0),
        ]);
        let mut source = RirEditor::new();
        source.add_inst(Inst {
            data: InstData::StringConst {
                content: source_symbol,
                anchor: anchor.clone(),
            },
            span: Span::with_file(FileId::new(7), 3, 11),
        });
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 20)],
            },
        )
        .unwrap();
        let mut destination = RirEditor::new();
        destination
            .append_remapped_with_spans(
                &source,
                |_| destination_symbol,
                |span| Span::with_file(FileId::new(9), span.start + 20, span.end + 20),
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 100)],
            },
        )
        .unwrap();
        let (_, instruction) = destination.iter().next().unwrap();
        let InstData::StringConst {
            content,
            anchor: remapped_anchor,
        } = &instruction.data
        else {
            panic!("expected string const")
        };
        assert_eq!(*content, destination_symbol);
        assert_eq!(*remapped_anchor, anchor);
        assert_eq!(instruction.span, Span::with_file(FileId::new(9), 23, 31));
    }

    #[test]
    fn float_const_text_is_remapped_across_symbol_domains() {
        // Module merging re-homes every instruction into the program-wide RIR,
        // translating owner-local symbols as it goes. A `FloatConst`'s text is
        // a symbol, so it must be translated rather than copied — a merged
        // program that kept the source symbol would resolve the literal
        // against the wrong interner (ADR-0065 §3, RUE-1069).
        let interner = ThreadedRodeo::new();
        let source_symbol = interner.get_or_intern("6.022e23");
        let destination_symbol = interner.get_or_intern("0.5");
        let mut source = RirEditor::new();
        source.add_inst(Inst {
            data: InstData::FloatConst {
                text: source_symbol,
            },
            span: Span::with_file(FileId::new(7), 3, 11),
        });
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 20)],
            },
        )
        .unwrap();

        let mut destination = RirEditor::new();
        destination
            .append_remapped_with_spans(
                &source,
                |_| destination_symbol,
                |span| Span::with_file(FileId::new(9), span.start + 20, span.end + 20),
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 100)],
            },
        )
        .unwrap();

        let (_, instruction) = destination.iter().next().unwrap();
        let InstData::FloatConst { text } = &instruction.data else {
            panic!("expected a float const, got {:?}", instruction.data);
        };
        assert_eq!(*text, destination_symbol);
        assert_eq!(instruction.span, Span::with_file(FileId::new(9), 23, 31));
    }

    #[test]
    fn float_const_symbol_is_validated_like_every_other_symbol_payload() {
        // A `FloatConst` whose text symbol is outside the compilation's
        // interner is a malformed producer request, caught by RIR validation
        // rather than surfacing as a bogus literal downstream.
        let mut rir = RirEditor::new();
        rir.add_inst(Inst {
            data: InstData::FloatConst {
                text: Spur::try_from_usize(41).unwrap(),
            },
            span: Span::with_file(FileId::new(0), 0, 3),
        });
        let error = ValidatedRir::finish(
            rir,
            &RirValidationContext {
                symbol_count: 3,
                source_lengths: &[(FileId::new(0), 20)],
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("symbol"),
            "unexpected validation error: {error}"
        );
    }

    #[test]
    fn empty_payloads_are_canonical_and_borrowed_views_are_empty() {
        let mut rir = Rir::new();
        let call = rir.add_call_args(&[]).unwrap();
        let params = rir.add_params(&[]).unwrap();
        let arms = rir.add_match_arms(&[]).unwrap();
        let directives = rir.add_directives(&[]).unwrap();
        assert_eq!(rir.extra_len(), 0);
        assert!(rir.call_args(&call).is_empty());
        assert!(rir.params(&params).is_empty());
        assert!(rir.match_arms(&arms).is_empty());
        assert!(rir.directives(&directives).is_empty());
    }

    #[test]
    fn validation_reports_family_range_and_record_deterministically() {
        let mut rir = Rir::new();
        rir.extra.extend([1, PatternKind::Path as u32]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = rir.validate_payloads().unwrap_err();
        assert_eq!(
            error,
            rir_payload_error! {
                family: "match arms",
                start: 0,
                extent: 2,
                record: Some(0),
                expected: MATCH_PATH_BINDING_COUNT + 1,
                actual: 1,
                reason: "path record header is truncated",
            }
        );
        assert_eq!(error.phase(), "RIR payload decode");
        assert_eq!(error.expected_width(), MATCH_PATH_BINDING_COUNT + 1);
        assert_eq!(error.actual_width(), 1);
        let rendered = error.to_string();
        assert!(rendered.contains("match arms"));
        assert!(rendered.contains("start=0"));
        assert!(rendered.contains("record 0"));
        assert!(rendered.contains(&format!(
            "expected width={}, actual width=1",
            MATCH_PATH_BINDING_COUNT + 1
        )));
    }

    #[test]
    fn validation_rejects_noncanonical_empty_ranges() {
        let mut rir = Rir::new();
        let args = RirCallArgsRange::from_parts(1, 0);
        rir.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        assert_eq!(
            rir.validate_payloads().unwrap_err().reason,
            "noncanonical empty range"
        );
    }

    #[test]
    fn validation_rejects_partial_fixed_records_and_invalid_modes() {
        let mut partial = Rir::new();
        partial.extra.push(0);
        let args = RirCallArgsRange::from_parts(0, 1);
        partial.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        let error = partial.validate_payloads().unwrap_err();
        assert_eq!(error.reason, "payload ends in a partial record");
        assert_eq!(
            (error.expected_width(), error.actual_width()),
            (CALL_ARG_SCHEMA.width, 1)
        );

        let mut invalid_mode = Rir::new();
        invalid_mode.extra.extend([0, 99]);
        let args = RirCallArgsRange::from_parts(0, 2);
        invalid_mode.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        assert_eq!(
            invalid_mode.validate_payloads().unwrap_err().reason,
            "invalid argument mode"
        );
    }

    #[test]
    fn validation_rejects_unknown_tags_trailing_words_and_bad_enum_cardinality() {
        let mut unknown = Rir::new();
        unknown.extra.extend([1, 99]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        unknown.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = unknown.validate_payloads().unwrap_err();
        assert_eq!(error.reason, "invalid pattern kind");
        assert_eq!((error.expected_width(), error.actual_width()), (1, 1));

        let mut trailing = Rir::new();
        trailing.extra.extend([0, 7]);
        let directives = RirDirectivesRange::from_parts(0, 2);
        trailing.add_inst(Inst {
            data: InstData::ConstDecl {
                directives,
                is_pub: false,
                name: Spur::default(),
                ty: None,
                init: InstRef::from_raw(0),
            },
            span: span(),
        });
        assert_eq!(
            trailing.validate_payloads().unwrap_err().reason,
            "trailing words after final record"
        );

        let mut cardinality = Rir::new();
        cardinality.extra.extend([0, 0, 7]);
        let variants = RirEnumVariantsRange::from_parts(0, 1);
        let payloads = RirEnumPayloadsRange::from_parts(1, 2);
        cardinality.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: Spur::default(),
                variants,
                payloads,
            },
            span: span(),
        });
        assert_eq!(
            cardinality.validate_payloads().unwrap_err().reason,
            "trailing words after variant payloads"
        );
    }

    fn context() -> RirValidationContext<'static> {
        static SOURCES: [(FileId, u32); 1] = [(FileId::new(7), 100)];
        RirValidationContext {
            symbol_count: 1,
            source_lengths: &SOURCES,
        }
    }

    #[test]
    fn finish_rejects_noncanonical_match_scalars_before_iteration() {
        let mut boolean = RirEditor::new();
        let value = boolean.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        boolean
            .add_match(value, &[(RirPattern::Bool(true, span()), value)], span())
            .unwrap();
        boolean.rir.extra[5] = 2;
        assert_eq!(
            ValidatedRir::finish(boolean, &context())
                .unwrap_err()
                .reason,
            "invalid boolean scalar"
        );

        let mut integer = RirEditor::new();
        let value = integer.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        integer
            .add_match(
                value,
                &[(
                    RirPattern::Int {
                        value: 1,
                        negative: false,
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        integer.rir.extra[7] = 2;
        assert_eq!(
            ValidatedRir::finish(integer, &context())
                .unwrap_err()
                .reason,
            "invalid integer-sign flag"
        );
    }

    #[test]
    fn finish_rejects_unrepresentable_directive_argument_before_iteration() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        editor
            .add_const_decl(
                &[RirDirective {
                    name: symbol,
                    args: vec![symbol],
                    span: span(),
                }],
                false,
                symbol,
                None,
                value,
                span(),
            )
            .unwrap();
        editor.rir.extra[DIRECTIVE_ARGS_START + 1] = u32::MAX;

        assert_eq!(
            ValidatedRir::finish(editor, &context()).unwrap_err(),
            rir_payload_error! {
                family: RirDirectivesRange::FAMILY,
                start: 0,
                extent: 7,
                record: Some(0),
                expected: 6,
                actual: 6,
                reason: "symbol word is not representable",
            }
        );
    }

    #[test]
    fn finish_rejects_unrepresentable_match_binding_before_iteration() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        editor
            .add_match(
                value,
                &[(
                    RirPattern::Path {
                        module: None,
                        ctor_head: None,
                        type_name: symbol,
                        variant: symbol,
                        bindings: vec![symbol],
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        editor.rir.extra[MATCH_PATH_BINDINGS_START + 1] = u32::MAX;

        assert_eq!(
            ValidatedRir::finish(editor, &context()).unwrap_err(),
            rir_payload_error! {
                family: RirMatchArmsRange::FAMILY,
                start: 0,
                extent: 12,
                record: Some(0),
                expected: 11,
                actual: 11,
                reason: "symbol word is not representable",
            }
        );
    }

    #[test]
    fn finish_rejects_out_of_owner_match_refs_and_context_values() {
        let symbol = Spur::try_from_usize(0).unwrap();
        for (module, ctor, body) in [
            (Some(InstRef::from_raw(99)), None, InstRef::from_raw(0)),
            (None, Some(InstRef::from_raw(99)), InstRef::from_raw(0)),
            (None, None, InstRef::from_raw(99)),
        ] {
            let mut editor = RirEditor::new();
            let scrutinee = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: span(),
            });
            editor
                .add_match(
                    scrutinee,
                    &[(
                        RirPattern::Path {
                            module,
                            ctor_head: ctor,
                            type_name: symbol,
                            variant: symbol,
                            bindings: vec![],
                            span: span(),
                        },
                        body,
                    )],
                    span(),
                )
                .unwrap();
            assert_eq!(
                ValidatedRir::finish(editor, &context()).unwrap_err().reason,
                "instruction reference is outside the owner"
            );
        }

        let mut bad_symbol = RirEditor::new();
        bad_symbol.add_inst(Inst {
            data: InstData::StringConst {
                content: Spur::try_from_usize(77).unwrap(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::StringLiteral(0)]),
            },
            span: span(),
        });
        assert_eq!(
            ValidatedRir::finish(bad_symbol, &context())
                .unwrap_err()
                .reason,
            "symbol is outside the canonical interner"
        );

        let mut bad_symbol_word = RirEditor::new();
        bad_symbol_word.rir.extra.push(77);
        bad_symbol_word.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: symbol,
                variants: RirEnumVariantsRange::from_parts(0, 1),
                payloads: RirEnumPayloadsRange::payload_fallback(),
            },
            span: span(),
        });
        assert_eq!(
            ValidatedRir::finish(bad_symbol_word, &context())
                .unwrap_err()
                .reason,
            "symbol is outside the canonical interner"
        );

        let mut overflow = RirEditor::new();
        let value = overflow.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        overflow
            .add_match(value, &[(RirPattern::Wildcard(span()), value)], span())
            .unwrap();
        overflow.rir.extra[2] = u32::MAX;
        overflow.rir.extra[3] = 1;
        assert_eq!(
            ValidatedRir::finish(overflow, &context())
                .unwrap_err()
                .reason,
            "pattern span overflows u32"
        );
    }

    #[test]
    fn borrowed_payload_traversal_allocates_nothing() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let mut rir = Rir::new();
        let type_a = install_named_types(&mut rir, &[a])[0];
        let refs = rir
            .add_block_insts(&[InstRef::from_raw(0), InstRef::from_raw(1)])
            .unwrap();
        let intrinsic = rir.add_intrinsic_args(&[InstRef::from_raw(0)]).unwrap();
        let internal = rir
            .add_internal_intrinsic_args(&[InstRef::from_raw(0)])
            .unwrap();
        let methods = rir.add_struct_methods(&[InstRef::from_raw(0)]).unwrap();
        let anon_methods = rir
            .add_anon_struct_methods(&[InstRef::from_raw(0)])
            .unwrap();
        let elements = rir.add_array_elements(&[InstRef::from_raw(0)]).unwrap();
        let calls = rir
            .add_call_args(&[RirCallArg {
                value: InstRef::from_raw(0),
                mode: RirArgMode::Normal,
            }])
            .unwrap();
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![a],
                span: span(),
            }])
            .unwrap();
        let arms = rir
            .add_match_arms(&[(RirPattern::Wildcard(span()), InstRef::from_raw(1))])
            .unwrap();
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_a,
                mode: RirParamMode::Normal,
                is_comptime: false,
                span: span(),
            }])
            .unwrap();
        let inits = rir.add_field_inits(&[(a, InstRef::from_raw(0))]).unwrap();
        let fields = rir.add_struct_fields(&[(a, type_a)]).unwrap();
        let anon_fields = rir.add_anon_struct_fields(&[(a, type_a)]).unwrap();
        let variants = rir.add_enum_variants(&[a]).unwrap();
        let anon_variants = rir.add_anon_enum_variants(&[a]).unwrap();
        let payloads = rir.add_enum_payloads(&[vec![type_a]]).unwrap();
        let anon_payloads = rir.add_anon_enum_payloads(&[vec![type_a]]).unwrap();
        assert_eq!(
            allocations_during(|| {
                std::hint::black_box(rir.block_insts(&refs).values().count());
                std::hint::black_box(rir.intrinsic_args(&intrinsic).values().count());
                std::hint::black_box(rir.internal_intrinsic_args(&internal).values().count());
                std::hint::black_box(rir.struct_methods(&methods).values().count());
                std::hint::black_box(rir.anon_struct_methods(&anon_methods).values().count());
                std::hint::black_box(rir.array_elements(&elements).values().count());
                std::hint::black_box(rir.call_args(&calls).values().count());
                std::hint::black_box(rir.params(&params).values().count());
                std::hint::black_box(rir.directives(&directives).iter().count());
                std::hint::black_box(rir.match_arms(&arms).iter().count());
                std::hint::black_box(rir.field_inits(&inits).values().count());
                std::hint::black_box(rir.struct_fields(&fields).values().count());
                std::hint::black_box(rir.anon_struct_fields(&anon_fields).values().count());
                std::hint::black_box(rir.enum_variants(&variants).values().count());
                std::hint::black_box(rir.anon_enum_variants(&anon_variants).values().count());
                std::hint::black_box(rir.enum_payloads(&payloads, &variants).flatten().count());
                std::hint::black_box(
                    rir.anon_enum_payloads(&anon_payloads, &anon_variants)
                        .flatten()
                        .count(),
                );
            }),
            0
        );
    }

    #[test]
    fn every_symbol_bearing_schema_rejects_u32_max_before_views() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let reference = InstRef::from_raw(0);
        let assert_rejected = |mut rir: Rir, corrupt: usize, data: InstData| {
            rir.extra[corrupt] = u32::MAX;
            rir.add_inst(Inst { data, span: span() });
            let error = rir.validate_payloads().unwrap_err();
            assert!(
                error.reason.contains("symbol") || error.reason.contains("schema"),
                "{error:?}"
            );
        };

        let mut rir = Rir::new();
        let type_a = install_named_types(&mut rir, &[a])[0];
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_a,
                mode: RirParamMode::Normal,
                is_comptime: false,
                span: span(),
            }])
            .unwrap();
        assert_rejected(
            rir,
            0,
            InstData::FnDecl {
                directives: RirDirectivesRange::payload_fallback(),
                is_pub: false,
                is_unchecked: false,
                is_extern: false,
                is_c_export: false,
                name: a,
                params,
                return_type: type_a,
                body: reference,
                has_self: false,
                self_mode: RirParamMode::Normal,
                self_is_mut: false,
                returns_borrow: false,
                returns_inout: false,
            },
        );

        let mut rir = Rir::new();
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![a],
                span: span(),
            }])
            .unwrap();
        assert_rejected(
            rir,
            1,
            InstData::ConstDecl {
                directives,
                is_pub: false,
                name: a,
                ty: None,
                init: reference,
            },
        );

        let mut rir = Rir::new();
        let arms = rir
            .add_match_arms(&[(
                RirPattern::Path {
                    module: None,
                    ctor_head: None,
                    type_name: a,
                    variant: a,
                    bindings: vec![a],
                    span: span(),
                },
                reference,
            )])
            .unwrap();
        assert_rejected(
            rir,
            7,
            InstData::Match {
                scrutinee: reference,
                arms,
            },
        );

        macro_rules! fixed_symbol_case {
            ($builder:expr, $data:expr) => {{
                let mut rir = Rir::new();
                let _ = install_named_types(&mut rir, &[a]);
                let range = ($builder)(&mut rir);
                assert_rejected(rir, 0, ($data)(range));
            }};
        }
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_field_inits(&[(a, reference)]).unwrap(),
            |fields| InstData::StructInit {
                module: None,
                ctor_head: None,
                type_name: a,
                fields,
                shorthand_span: None,
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_struct_fields(&[(a, type_a)]).unwrap(),
            |fields| InstData::StructDecl {
                directives: RirDirectivesRange::payload_fallback(),
                is_pub: false,
                is_linear: false,
                name: a,
                fields,
                methods: RirStructMethodsRange::payload_fallback(),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_anon_struct_fields(&[(a, type_a)]).unwrap(),
            |fields| InstData::AnonStructType {
                fields,
                methods: RirAnonStructMethodsRange::payload_fallback(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0),]),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_enum_variants(&[a]).unwrap(),
            |variants| InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: a,
                variants,
                payloads: RirEnumPayloadsRange::payload_fallback(),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_anon_enum_variants(&[a]).unwrap(),
            |variants| InstData::AnonEnumType {
                variants,
                payloads: RirAnonEnumPayloadsRange::payload_fallback(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0),]),
            }
        );
    }

    #[test]
    fn every_payload_builder_records_per_family_allocation_and_storage_evidence() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let type_a = RirTypeSyntaxRef::from_u32(0);
        let type_b = RirTypeSyntaxRef::from_u32(1);
        let r0 = InstRef::from_raw(0);
        let r1 = InstRef::from_raw(1);
        let directives = [RirDirective {
            name: a,
            args: vec![b],
            span: span(),
        }];
        let params = [RirParam {
            name: a,
            ty: type_b,
            mode: RirParamMode::Borrow,
            is_comptime: true,
            span: span(),
        }];
        let calls = [RirCallArg {
            value: r0,
            mode: RirArgMode::Inout,
        }];
        let enum_payloads = [vec![type_a], vec![]];
        let anon_payloads = [vec![type_b]];
        #[derive(Debug)]
        struct Evidence {
            family: &'static str,
            allocation_calls: usize,
            allocated_bytes: usize,
            logical_bytes: usize,
            retained_capacity_bytes: usize,
            elements: usize,
            build_ns: u128,
            build_elements_per_second: f64,
            traversal_ns: u128,
            elements_per_second: f64,
            peak_staging_bytes: usize,
        }
        macro_rules! evidence {
            ($family:expr, $build:expr, $consume:expr) => {{
                let mut rir = Rir::new();
                let installed = install_named_types(&mut rir, &[a, b]);
                assert_eq!(installed, [type_a, type_b]);
                let mut range = None;
                let build_started = std::time::Instant::now();
                let (allocation_calls, allocated_bytes) = allocation_evidence(|| {
                    range = Some(($build)(&mut rir));
                });
                let build_ns = build_started.elapsed().as_nanos();
                let range = range.unwrap();
                const TRAVERSALS: usize = 20_000;
                let started = std::time::Instant::now();
                let mut consumed = 0usize;
                for _ in 0..TRAVERSALS {
                    consumed += std::hint::black_box(($consume)(&rir, &range));
                }
                let traversal_ns = started.elapsed().as_nanos();
                let elements = consumed / TRAVERSALS;
                let logical_bytes = rir.extra.len() * std::mem::size_of::<u32>();
                let peak_staging_bytes = match $family {
                    RirIntrinsicArgsRange::FAMILY
                    | RirInternalIntrinsicArgsRange::FAMILY
                    | RirBlockInstsRange::FAMILY
                    | RirStructMethodsRange::FAMILY
                    | RirAnonStructMethodsRange::FAMILY
                    | RirArrayElemsRange::FAMILY
                    | RirCallArgsRange::FAMILY
                    | RirParamsRange::FAMILY
                    | RirMatchArmsRange::FAMILY
                    | RirFieldInitsRange::FAMILY
                    | RirStructFieldsRange::FAMILY
                    | RirAnonStructFieldsRange::FAMILY => 0,
                    _ => logical_bytes,
                };
                Evidence {
                    family: $family,
                    allocation_calls,
                    allocated_bytes,
                    logical_bytes,
                    retained_capacity_bytes: rir.extra.capacity() * std::mem::size_of::<u32>(),
                    elements,
                    build_ns,
                    build_elements_per_second: elements as f64 / (build_ns as f64 / 1e9),
                    traversal_ns,
                    elements_per_second: consumed as f64 / (traversal_ns as f64 / 1e9),
                    peak_staging_bytes,
                }
            }};
        }
        let evidence = [
            evidence!(
                RirIntrinsicArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_intrinsic_args(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.intrinsic_args(range).len()
            ),
            evidence!(
                RirInternalIntrinsicArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_internal_intrinsic_args(&[r0]).unwrap() },
                |rir: &Rir, range| rir.internal_intrinsic_args(range).len()
            ),
            evidence!(
                RirBlockInstsRange::FAMILY,
                |rir: &mut Rir| { rir.add_block_insts(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.block_insts(range).len()
            ),
            evidence!(
                RirStructMethodsRange::FAMILY,
                |rir: &mut Rir| { rir.add_struct_methods(&[r0]).unwrap() },
                |rir: &Rir, range| rir.struct_methods(range).len()
            ),
            evidence!(
                RirAnonStructMethodsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_struct_methods(&[r1]).unwrap() },
                |rir: &Rir, range| rir.anon_struct_methods(range).len()
            ),
            evidence!(
                RirArrayElemsRange::FAMILY,
                |rir: &mut Rir| { rir.add_array_elements(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.array_elements(range).len()
            ),
            evidence!(
                RirCallArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_call_args(&calls).unwrap() },
                |rir: &Rir, range| rir.call_args(range).len()
            ),
            evidence!(
                RirParamsRange::FAMILY,
                |rir: &mut Rir| { rir.add_params(&params).unwrap() },
                |rir: &Rir, range| rir.params(range).len()
            ),
            evidence!(
                RirMatchArmsRange::FAMILY,
                |rir: &mut Rir| {
                    rir.add_match_arms(&[(RirPattern::Wildcard(span()), r0)])
                        .unwrap()
                },
                |rir: &Rir, range| rir.match_arms(range).len()
            ),
            evidence!(
                RirFieldInitsRange::FAMILY,
                |rir: &mut Rir| { rir.add_field_inits(&[(a, r0)]).unwrap() },
                |rir: &Rir, range| rir.field_inits(range).len()
            ),
            evidence!(
                RirStructFieldsRange::FAMILY,
                |rir: &mut Rir| { rir.add_struct_fields(&[(a, type_b)]).unwrap() },
                |rir: &Rir, range| rir.struct_fields(range).len()
            ),
            evidence!(
                RirAnonStructFieldsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_struct_fields(&[(a, type_b)]).unwrap() },
                |rir: &Rir, range| rir.anon_struct_fields(range).len()
            ),
            evidence!(
                RirDirectivesRange::FAMILY,
                |rir: &mut Rir| { rir.add_directives(&directives).unwrap() },
                |rir: &Rir, range| rir.directives(range).len()
            ),
            evidence!(
                RirEnumVariantsRange::FAMILY,
                |rir: &mut Rir| { rir.add_enum_variants(&[a, b]).unwrap() },
                |rir: &Rir, range| rir.enum_variants(range).len()
            ),
            evidence!(
                RirAnonEnumVariantsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_enum_variants(&[b]).unwrap() },
                |rir: &Rir, range| rir.anon_enum_variants(range).len()
            ),
            evidence!(
                RirEnumPayloadsRange::FAMILY,
                |rir: &mut Rir| { rir.add_enum_payloads(&enum_payloads).unwrap() },
                |rir: &Rir, range| rir
                    .enum_payloads(range, &RirEnumVariantsRange::from_parts(0, 2))
                    .map(|v| v.len())
                    .sum::<usize>()
            ),
            evidence!(
                RirAnonEnumPayloadsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_enum_payloads(&anon_payloads).unwrap() },
                |rir: &Rir, range| rir
                    .anon_enum_payloads(range, &RirAnonEnumVariantsRange::from_parts(0, 1))
                    .map(|v| v.len())
                    .sum::<usize>()
            ),
        ];
        assert_eq!(evidence.len(), 17);
        for item in &evidence {
            let minimum_allocations = if item.peak_staging_bytes == 0 { 1 } else { 2 };
            assert!(
                item.allocation_calls >= minimum_allocations,
                "{}: {item:?}",
                item.family
            );
            assert!(item.logical_bytes > 0, "{}: {item:?}", item.family);
            assert!(
                item.retained_capacity_bytes >= item.logical_bytes,
                "{}: {item:?}",
                item.family
            );
            assert!(
                item.allocated_bytes >= item.retained_capacity_bytes,
                "{}: {item:?}",
                item.family
            );
            assert!(item.elements > 0 && item.traversal_ns > 0);
            assert!(item.elements_per_second.is_finite());
            assert!(item.build_ns > 0 && item.build_elements_per_second.is_finite());
            assert!(item.peak_staging_bytes == 0 || item.peak_staging_bytes == item.logical_bytes);
            eprintln!(
                "RUE843_FAMILY\tphase=RIR\tfamily={}\telements={}\tbuild_ns={}\tbuild_elements_per_second={}\tbuild_allocations={}\tbuild_allocated_bytes={}\ttraversal_ns={}\ttraversal_elements_per_second={}\ttraversal_allocations=0\tlogical_bytes={}\tcapacity_bytes={}\ttotal_bytes={}\tenvelopes={}\tpeak_staging_bytes={}",
                item.family,
                item.elements,
                item.build_ns,
                item.build_elements_per_second,
                item.allocation_calls,
                item.allocated_bytes,
                item.traversal_ns,
                item.elements_per_second,
                item.logical_bytes,
                item.retained_capacity_bytes,
                item.logical_bytes + item.retained_capacity_bytes,
                usize::from(matches!(
                    item.family,
                    "match arms"
                        | "directives"
                        | "enum variant payloads"
                        | "anonymous enum variant payloads"
                )),
                item.peak_staging_bytes,
            );
        }
        eprintln!("RUE-843 RIR family evidence: {evidence:#?}");
        std::hint::black_box(evidence);
    }
}
