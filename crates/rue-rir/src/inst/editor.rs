//! Mutable construction, typed appends, and owner-local remapping.

use super::*;

#[path = "packed.rs"]
mod packed;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use packed::{
    PackedRirAppend, PackedRirAppendError, PackedRirAppendMetadata, PackedRirDecodeError,
    PackedRirEncodeError, PackedRirMetadata, PackedRirMethodOwner, PackedRirProjection,
    PackedRirSymbols, PackedValidatedRir, RirFallibleIntrinsic, RirFallibleIntrinsicSet,
};

fn type_syntax_build_error(error: crate::RirTypeSyntaxBuildError) -> RirPayloadBuildError {
    let family = match error {
        crate::RirTypeSyntaxBuildError::TooManyNodes => "type syntax nodes",
        crate::RirTypeSyntaxBuildError::TooManySymbols => "type syntax symbols",
        crate::RirTypeSyntaxBuildError::TooMuchPayload => "type syntax payload",
    };
    RirPayloadBuildError::ResourceLimitExceeded { family }
}

/// Mutable construction-phase owner. Payload descriptors never leave this
/// owner through the public API; callers add or replace complete nodes.
///
/// Family identities cannot be interchanged:
///
/// ```compile_fail
/// use rue_rir::{Rir, RirParamsRange};
/// fn wrong_family(rir: &Rir, params: &RirParamsRange) {
///     let _ = rir.call_args(params);
/// }
/// ```
///
/// Raw positions cannot be reconstructed:
///
/// ```compile_fail
/// use rue_rir::RirCallArgsRange;
/// let _ = RirCallArgsRange::from_parts(0, 0);
/// ```
///
/// A descriptor cannot be extracted from a published owner for movement to a
/// different editor:
///
/// ```compile_fail
/// use rue_rir::{InstData, InstRef, Rir, RirCallArgsRange};
/// fn extract(rir: &Rir, inst: InstRef) -> RirCallArgsRange {
///     match &rir.get_inst(inst).data {
///         InstData::Call { args, .. } => *args,
///         _ => panic!("not a call"),
///     }
/// }
/// ```
///
/// Consequently a payload-bearing node cannot be detached from one owner and
/// inserted into another:
///
/// ```compile_fail
/// use rue_rir::{Inst, InstData, InstRef, Rir, RirEditor};
/// fn detach(source: &Rir, destination: &mut RirEditor, inst: InstRef) {
///     let borrowed = source.get_inst(inst);
///     destination.add_inst(Inst { data: borrowed.data, span: borrowed.span });
/// }
/// ```
#[derive(Debug, Default)]
pub struct RirEditor {
    rir: Rir,
    type_syntax: RirTypeSyntaxBuilder<Spur>,
}

/// The destination ranges occupied by one RIR owner after a typed append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirAppendRange {
    pub instructions: std::ops::Range<u32>,
    pub extra: std::ops::Range<u32>,
}

struct StructMethodsOverride<'a> {
    source_root: InstRef,
    destination_methods: &'a [InstRef],
}

fn remap_call_args(
    args: RirSlice<'_, RirCallArg>,
    remap_ref: impl Fn(InstRef) -> InstRef,
) -> Vec<RirCallArg> {
    args.values()
        .map(|argument| RirCallArg {
            value: remap_ref(argument.value),
            mode: argument.mode,
        })
        .collect()
}

impl RirEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Project one parser type directly into this RIR owner's dense structured
    /// type arena. The supplied resolver transports parser-local spellings into
    /// the instruction graph's candidate-local symbol universe.
    pub fn add_parser_type(
        &mut self,
        ty: &rue_parser::ast::TypeExpr,
        resolve: impl Copy + Fn(Spur) -> Spur,
    ) -> Result<crate::RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_parser_type(ty, resolve)
    }

    pub fn add_unit_type(
        &mut self,
    ) -> Result<crate::RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_unit_type()
    }

    pub fn add_named_type(
        &mut self,
        symbol: Spur,
    ) -> Result<RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_named_type(symbol)
    }

    pub(crate) fn into_unvalidated(self) -> Rir {
        let Self {
            mut rir,
            type_syntax,
        } = self;
        rir.type_syntax = type_syntax.finish();
        rir
    }

    /// Finish the owner-mediated editor without contextual validation.
    ///
    /// This is the post-construction counterpart to [`AstGen::finish`], used
    /// by controlled synthesis that must make one final editor-only
    /// replacement before exposing the immutable RIR. Production publication
    /// should prefer [`ValidatedRir::finish`].
    #[doc(hidden)]
    pub fn finish(self) -> Rir {
        self.into_unvalidated()
    }

    fn atomic<T>(
        &mut self,
        build: impl FnOnce(&mut Rir) -> Result<T, RirPayloadBuildError>,
    ) -> Result<T, RirPayloadBuildError> {
        let instruction_len = self.rir.instructions.len();
        let extra_len = self.rir.extra.len();
        match build(&mut self.rir) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.rir.instructions.truncate(instruction_len);
                self.rir.extra.truncate(extra_len);
                Err(error)
            }
        }
    }

    /// Add a payload-free node. Payload-bearing nodes use the atomic methods
    /// below, whose descriptors never escape the editor.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        self.rir.add_inst(inst)
    }

    /// The implementation-limit rejection latched by an infallible
    /// [`Self::add_inst`] that ran past the published instruction ceiling
    /// (spec Appendix C.6:1), if any. Publication boundaries consult this so
    /// the ceiling becomes an `E1401` diagnostic rather than a wrapped
    /// `InstRef` (spec C.1:2).
    pub fn capacity_error(&self) -> Option<RirPayloadBuildError> {
        self.rir.latched_capacity_error()
    }

    pub fn add_intrinsic(
        &mut self,
        name: Spur,
        args: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_intrinsic_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Intrinsic { name, args },
                span,
            }))
        })
    }

    pub fn add_internal_intrinsic(
        &mut self,
        intrinsic: InternalIntrinsic,
        args: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_internal_intrinsic_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::InternalIntrinsic { intrinsic, args },
                span,
            }))
        })
    }

    pub fn add_block(
        &mut self,
        instructions: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let instructions = rir.add_block_insts(instructions)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Block { instructions },
                span,
            }))
        })
    }

    pub fn add_call(
        &mut self,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_call_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Call { name, args },
                span,
            }))
        })
    }

    pub fn add_method_call(
        &mut self,
        receiver: InstRef,
        method: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_method_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::MethodCall {
                    receiver,
                    method,
                    args,
                },
                span,
            }))
        })
    }

    pub fn add_match(
        &mut self,
        scrutinee: InstRef,
        arms: &[(RirPattern, InstRef)],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let arms = rir.add_match_arms(arms)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Match { scrutinee, arms },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn add_fn_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        name: Spur,
        params: &[RirParam],
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
        returns_borrow: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.add_fn_decl_with_return_modes(
            directives,
            is_pub,
            is_unchecked,
            is_extern,
            is_c_export,
            name,
            params,
            return_type,
            body,
            has_self,
            self_mode,
            self_is_mut,
            returns_borrow,
            false,
            span,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_fn_decl_with_return_modes(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        name: Spur,
        params: &[RirParam],
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
        returns_borrow: bool,
        returns_inout: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            let params = rir.add_params(params)?;
            Ok(rir.add_inst(Inst {
                data: InstData::FnDecl {
                    directives,
                    is_pub,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    returns_borrow,
                    returns_inout,
                },
                span,
            }))
        })
    }

    pub fn add_const_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        name: Spur,
        ty: Option<RirTypeSyntaxRef>,
        init: InstRef,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            Ok(rir.add_inst(Inst {
                data: InstData::ConstDecl {
                    directives,
                    is_pub,
                    name,
                    ty,
                    init,
                },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_alloc(
        &mut self,
        directives: &[RirDirective],
        name: Option<Spur>,
        is_mut: bool,
        ty: Option<RirTypeSyntaxRef>,
        init: InstRef,
        iter_elem: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Alloc {
                    directives,
                    name,
                    is_mut,
                    ty,
                    init,
                    iter_elem,
                },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_struct_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_linear: bool,
        name: Spur,
        fields: &[(Spur, RirTypeSyntaxRef)],
        methods: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            let fields = rir.add_struct_fields(fields)?;
            let methods = rir.add_struct_methods(methods)?;
            Ok(rir.add_inst(Inst {
                data: InstData::StructDecl {
                    directives,
                    is_pub,
                    is_linear,
                    name,
                    fields,
                    methods,
                },
                span,
            }))
        })
    }

    pub fn add_struct_init(
        &mut self,
        module: Option<InstRef>,
        ctor_head: Option<InstRef>,
        type_name: Spur,
        fields: &[(Spur, InstRef)],
        shorthand_span: Option<Span>,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let fields = rir.add_field_inits(fields)?;
            Ok(rir.add_inst(Inst {
                data: InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span,
                },
                span,
            }))
        })
    }

    pub fn add_enum_decl(
        &mut self,
        is_pub: bool,
        is_non_exhaustive: bool,
        name: Spur,
        variants: &[Spur],
        payloads: &[Vec<RirTypeSyntaxRef>],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            if variants.len() != payloads.len() {
                return Err(RirPayloadBuildError::InvalidBuilderInput {
                    family: RirEnumPayloadsRange::FAMILY,
                    reason: "variant and payload counts differ",
                });
            }
            let variants = rir.add_enum_variants(variants)?;
            let payloads = rir.add_enum_payloads(payloads)?;
            Ok(rir.add_inst(Inst {
                data: InstData::EnumDecl {
                    is_pub,
                    is_non_exhaustive,
                    name,
                    variants,
                    payloads,
                },
                span,
            }))
        })
    }

    pub fn add_array_init(
        &mut self,
        elements: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let elements = rir.add_array_elements(elements)?;
            Ok(rir.add_inst(Inst {
                data: InstData::ArrayInit { elements },
                span,
            }))
        })
    }

    pub fn add_anon_struct_type(
        &mut self,
        fields: &[(Spur, RirTypeSyntaxRef)],
        methods: &[InstRef],
        anchor: RirStructuralAnchor,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let fields = rir.add_anon_struct_fields(fields)?;
            let methods = rir.add_anon_struct_methods(methods)?;
            Ok(rir.add_inst(Inst {
                data: InstData::AnonStructType {
                    fields,
                    methods,
                    anchor,
                },
                span,
            }))
        })
    }

    pub fn add_anon_enum_type(
        &mut self,
        variants: &[Spur],
        payloads: &[Vec<RirTypeSyntaxRef>],
        anchor: RirStructuralAnchor,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            if variants.len() != payloads.len() {
                return Err(RirPayloadBuildError::InvalidBuilderInput {
                    family: RirAnonEnumPayloadsRange::FAMILY,
                    reason: "variant and payload counts differ",
                });
            }
            let variants = rir.add_anon_enum_variants(variants)?;
            let payloads = rir.add_anon_enum_payloads(payloads)?;
            Ok(rir.add_inst(Inst {
                data: InstData::AnonEnumType {
                    variants,
                    payloads,
                    anchor,
                },
                span,
            }))
        })
    }

    /// Append an immutable RIR owner while remapping its owner-local symbols.
    ///
    /// Payload descriptors never cross the owner boundary. Every variable
    /// payload is decoded through its typed view and rebuilt by the matching
    /// destination builder; instruction references are translated by the
    /// destination instruction offset.
    pub fn append_remapped(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
    ) -> Result<RirAppendRange, RirPayloadBuildError> {
        self.append_remapped_with_spans(source, symbol, std::convert::identity)
    }

    /// Append an immutable RIR owner while remapping owner-local symbols and
    /// rebinding every embedded source span into the destination file table.
    pub fn append_remapped_with_spans(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
        mut remap_span: impl FnMut(Span) -> Span,
    ) -> Result<RirAppendRange, RirPayloadBuildError> {
        match self.try_append_remapped_with_span_slots(
            source,
            symbol,
            || Ok::<_, std::convert::Infallible>(()),
            |_, span| Ok::<_, std::convert::Infallible>(remap_span(span)),
        ) {
            Ok(range) => Ok(range),
            Err(RirSpanRemapError::Build(error)) => Err(error),
            Err(
                RirSpanRemapError::MalformedPayload(_)
                | RirSpanRemapError::MalformedTypeSyntax(_)
                | RirSpanRemapError::DuplicateSlot(_)
                | RirSpanRemapError::MissingSlot(_)
                | RirSpanRemapError::UnexpectedSlot { .. }
                | RirSpanRemapError::UnconsumedSlot(_)
                | RirSpanRemapError::InvalidInstructionRange(_)
                | RirSpanRemapError::ForeignInstructionEdge { .. },
            ) => unreachable!("validated RIR and canonical span schema must agree"),
            Err(RirSpanRemapError::Checkpoint(error))
            | Err(RirSpanRemapError::Mapping { error, .. }) => match error {},
        }
    }

    /// Atomically append an immutable RIR owner while fallibly remapping each
    /// span by its stable structural slot.
    ///
    /// The callback is evaluated by the canonical span visitor before the
    /// append begins. Checkpoints continue during rebuilding; cancellation or
    /// any error rolls the destination back to its original instruction and
    /// payload lengths.
    pub fn try_append_remapped_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        self.try_append_remapped_selection_with_span_slots(
            source, None, None, symbol, checkpoint, remap_span,
        )
    }

    /// Atomically append one methodless `StructDecl` shell while wiring it to
    /// method declarations that have already been composed in this editor.
    ///
    /// Candidate-local AstGen emits a struct shell independently from its
    /// methods. This is the sole composition seam: every directive, field,
    /// symbol, and span is rebuilt through the ordinary typed remapper, while
    /// only the empty methods payload is replaced. The source must contain
    /// exactly the supplied `StructDecl` root and no methods, and every
    /// replacement must name an existing destination `FnDecl`.
    pub fn try_append_methodless_struct_shell_with_methods<E>(
        &mut self,
        source: &ValidatedRir,
        source_root: InstRef,
        destination_methods: &[InstRef],
        symbol: impl FnMut(Spur) -> Spur,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        let invalid = |reason| {
            RirSpanRemapError::Build(RirPayloadBuildError::InvalidBuilderInput {
                family: "struct shell composition",
                reason,
            })
        };
        if usize::try_from(source_root.as_u32())
            .ok()
            .is_none_or(|root| root >= source.len())
        {
            return Err(invalid("source root is outside the candidate shell"));
        }
        let InstData::StructDecl { methods, .. } = &source.get(source_root).data else {
            return Err(invalid("source root is not a struct declaration"));
        };
        if source.struct_methods(methods).len() != 0 {
            return Err(invalid("source struct declaration is not methodless"));
        }
        if source.len() != 1 || source_root.as_u32() != 0 {
            return Err(invalid(
                "source candidate shell is not exactly one struct declaration",
            ));
        }
        if destination_methods.iter().any(|method| {
            !matches!(
                self.rir.instructions.get(method.as_u32() as usize),
                Some(Inst {
                    data: InstData::FnDecl { .. },
                    ..
                })
            )
        }) {
            return Err(invalid(
                "replacement method is not an existing destination function declaration",
            ));
        }
        self.try_append_remapped_selection_with_span_slots(
            source,
            None,
            Some(StructMethodsOverride {
                source_root,
                destination_methods,
            }),
            symbol,
            checkpoint,
            remap_span,
        )
    }

    /// Atomically copy one validated declaration-producer interval.
    ///
    /// Canonical AstGen records this interval around the producer call and the
    /// module publisher proves every child edge remains within it. Projection
    /// work is therefore proportional to this declaration, independent of the
    /// number or size of sibling declarations.
    pub fn try_append_instruction_range_remapped_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        instructions: std::ops::Range<u32>,
        symbol: impl FnMut(Spur) -> Spur,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        if instructions.start >= instructions.end
            || usize::try_from(instructions.end)
                .ok()
                .is_none_or(|end| end > source.len())
        {
            return Err(RirSpanRemapError::InvalidInstructionRange(instructions));
        }
        let mut children = Vec::new();
        for ordinal in instructions.clone() {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let instruction = InstRef::from_raw(ordinal);
            children.clear();
            source.child_instructions(instruction, &mut children);
            if let Some(child) = children.iter().copied().find(|child| {
                child.as_u32() < instructions.start || child.as_u32() >= instructions.end
            }) {
                return Err(RirSpanRemapError::ForeignInstructionEdge { instruction, child });
            }
        }
        self.try_append_remapped_selection_with_span_slots(
            source,
            Some(instructions),
            None,
            symbol,
            checkpoint,
            remap_span,
        )
    }

    fn try_append_remapped_selection_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        selected: Option<std::ops::Range<u32>>,
        struct_methods_override: Option<StructMethodsOverride<'_>>,
        mut symbol: impl FnMut(Spur) -> Spur,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        enum CollectError<E> {
            Checkpoint(E),
            Mapping { slot: RirSpanSlot, error: E },
        }

        let instruction_start = u32::try_from(self.rir.instructions.len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "instructions",
            }
        })?;
        let source_start = selected.as_ref().map_or(0, |range| range.start);
        let source_end = selected.as_ref().map_or_else(
            || u32::try_from(source.len()).unwrap_or(u32::MAX),
            |range| range.end,
        );
        let source_instructions = source_end - source_start;
        instruction_start.checked_add(source_instructions).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "instructions",
            },
        )?;

        let mut mapped_spans = Vec::new();
        let traversal = source.try_visit_instruction_range_span_slots(
            source_start..source_end,
            || checkpoint().map_err(CollectError::Checkpoint),
            |slot, span| {
                let destination = InstRef::from_raw(
                    instruction_start + (slot.instruction().as_u32() - source_start),
                );
                let destination_slot = RirSpanSlot::new(destination, slot.field());
                let mapped =
                    remap_span(destination_slot, span).map_err(|error| CollectError::Mapping {
                        slot: destination_slot,
                        error,
                    })?;
                mapped_spans.push((slot, mapped));
                Ok(())
            },
        );
        if let Err(error) = traversal {
            return Err(match error {
                RirSpanTraversalError::MalformedPayload(error) => {
                    RirSpanRemapError::MalformedPayload(error)
                }
                RirSpanTraversalError::DuplicateSlot(slot) => {
                    RirSpanRemapError::DuplicateSlot(slot)
                }
                RirSpanTraversalError::Callback(CollectError::Checkpoint(error)) => {
                    RirSpanRemapError::Checkpoint(error)
                }
                RirSpanTraversalError::Callback(CollectError::Mapping { slot, error }) => {
                    RirSpanRemapError::Mapping { slot, error }
                }
            });
        }
        let mut mapped_spans = mapped_spans.into_iter();
        let extra_start = u32::try_from(self.rir.extra.len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            }
        })?;
        let source_extra = u32::try_from(source.extra_len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            }
        })?;
        extra_start.checked_add(source_extra).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            },
        )?;
        let remap_ref =
            |value: InstRef| InstRef::from_raw(instruction_start + (value.as_u32() - source_start));
        let type_snapshot = self.type_syntax.snapshot();
        let type_map = match self.type_syntax.append_remapped(
            source.type_syntax(),
            |source_symbol| symbol(*source_symbol),
            || checkpoint(),
        ) {
            Ok(type_map) => type_map,
            Err(crate::RirTypeSyntaxAppendError::Malformed(error)) => {
                return Err(RirSpanRemapError::MalformedTypeSyntax(error));
            }
            Err(crate::RirTypeSyntaxAppendError::Checkpoint(error)) => {
                return Err(RirSpanRemapError::Checkpoint(error));
            }
            Err(crate::RirTypeSyntaxAppendError::Build(error)) => {
                return Err(RirSpanRemapError::Build(type_syntax_build_error(error)));
            }
        };
        let remap_type = |reference: RirTypeSyntaxRef| {
            type_map
                .get(reference.index())
                .copied()
                .expect("validated type-syntax reference has a destination")
        };
        let result = (|| {
            for ordinal in source_start..source_end {
                let source_instruction = InstRef::from_raw(ordinal);
                let instruction = source.get(source_instruction);
                checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                let mut take_span = |field| {
                    let expected = RirSpanSlot::new(source_instruction, field);
                    let Some((actual, span)) = mapped_spans.next() else {
                        return Err(RirSpanRemapError::MissingSlot(expected));
                    };
                    if actual != expected {
                        return Err(RirSpanRemapError::UnexpectedSlot { expected, actual });
                    }
                    Ok(span)
                };
                let span = take_span(RirSpanField::Instruction)?;
                let payload_free = |data| Inst { data, span };
                match &instruction.data {
                    InstData::IntConst(value) => {
                        self.add_inst(payload_free(InstData::IntConst(*value)))
                    }
                    InstData::FloatConst { text } => {
                        self.add_inst(payload_free(InstData::FloatConst {
                            text: symbol(*text),
                        }))
                    }
                    InstData::BoolConst(value) => {
                        self.add_inst(payload_free(InstData::BoolConst(*value)))
                    }
                    InstData::StringConst { content, anchor } => {
                        self.add_inst(payload_free(InstData::StringConst {
                            content: symbol(*content),
                            anchor: anchor.clone(),
                        }))
                    }
                    InstData::UnitConst => self.add_inst(payload_free(InstData::UnitConst)),
                    InstData::Add { lhs, rhs } => self.add_inst(payload_free(InstData::Add {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Sub { lhs, rhs } => self.add_inst(payload_free(InstData::Sub {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Mul { lhs, rhs } => self.add_inst(payload_free(InstData::Mul {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Div { lhs, rhs } => self.add_inst(payload_free(InstData::Div {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Mod { lhs, rhs } => self.add_inst(payload_free(InstData::Mod {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Eq { lhs, rhs } => self.add_inst(payload_free(InstData::Eq {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Ne { lhs, rhs } => self.add_inst(payload_free(InstData::Ne {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Lt { lhs, rhs } => self.add_inst(payload_free(InstData::Lt {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Gt { lhs, rhs } => self.add_inst(payload_free(InstData::Gt {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Le { lhs, rhs } => self.add_inst(payload_free(InstData::Le {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Ge { lhs, rhs } => self.add_inst(payload_free(InstData::Ge {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::And { lhs, rhs } => self.add_inst(payload_free(InstData::And {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Or { lhs, rhs } => self.add_inst(payload_free(InstData::Or {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::BitAnd { lhs, rhs } => {
                        self.add_inst(payload_free(InstData::BitAnd {
                            lhs: remap_ref(*lhs),
                            rhs: remap_ref(*rhs),
                        }))
                    }
                    InstData::BitOr { lhs, rhs } => self.add_inst(payload_free(InstData::BitOr {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::BitXor { lhs, rhs } => {
                        self.add_inst(payload_free(InstData::BitXor {
                            lhs: remap_ref(*lhs),
                            rhs: remap_ref(*rhs),
                        }))
                    }
                    InstData::Shl { lhs, rhs } => self.add_inst(payload_free(InstData::Shl {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Shr { lhs, rhs } => self.add_inst(payload_free(InstData::Shr {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Neg { operand } => self.add_inst(payload_free(InstData::Neg {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Not { operand } => self.add_inst(payload_free(InstData::Not {
                        operand: remap_ref(*operand),
                    })),
                    InstData::BitNot { operand } => self.add_inst(payload_free(InstData::BitNot {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Try { operand } => self.add_inst(payload_free(InstData::Try {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Branch {
                        cond,
                        then_block,
                        else_block,
                    } => self.add_inst(payload_free(InstData::Branch {
                        cond: remap_ref(*cond),
                        then_block: remap_ref(*then_block),
                        else_block: else_block.map(remap_ref),
                    })),
                    InstData::Loop { cond, body } => self.add_inst(payload_free(InstData::Loop {
                        cond: remap_ref(*cond),
                        body: remap_ref(*body),
                    })),
                    InstData::InfiniteLoop { body, iter_borrow } => {
                        self.add_inst(payload_free(InstData::InfiniteLoop {
                            body: remap_ref(*body),
                            iter_borrow: iter_borrow.map(&mut symbol),
                        }))
                    }
                    InstData::Match { scrutinee, arms } => {
                        let arms = source
                            .match_arms(arms)
                            .iter()
                            .enumerate()
                            .map(|(arm, (pattern, body))| {
                                let pattern_span = take_span(RirSpanField::MatchPattern {
                                    arm: u32::try_from(arm)
                                        .expect("validated match-arm count is encoded as u32"),
                                })?;
                                let pattern = match pattern {
                                    RirPatternView::Wildcard(_) => {
                                        RirPattern::Wildcard(pattern_span)
                                    }
                                    RirPatternView::Int {
                                        value,
                                        negative,
                                        span: _,
                                    } => RirPattern::Int {
                                        value,
                                        negative,
                                        span: pattern_span,
                                    },
                                    RirPatternView::Bool(value, _) => {
                                        RirPattern::Bool(value, pattern_span)
                                    }
                                    RirPatternView::Path {
                                        module,
                                        ctor_head,
                                        type_name,
                                        variant,
                                        bindings,
                                        span: _,
                                    } => RirPattern::Path {
                                        module: module.map(remap_ref),
                                        ctor_head: ctor_head.map(remap_ref),
                                        type_name: symbol(type_name),
                                        variant: symbol(variant),
                                        bindings: bindings.values().map(&mut symbol).collect(),
                                        span: pattern_span,
                                    },
                                };
                                Ok((pattern, remap_ref(body)))
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_match(remap_ref(*scrutinee), &arms, span)?
                    }
                    InstData::Break { value } => self.add_inst(payload_free(InstData::Break {
                        value: value.map(remap_ref),
                    })),
                    InstData::Continue => self.add_inst(payload_free(InstData::Continue)),
                    InstData::FnDecl {
                        directives,
                        is_pub,
                        is_unchecked,
                        is_extern,
                        is_c_export,
                        name,
                        params,
                        return_type,
                        body,
                        has_self,
                        self_mode,
                        self_is_mut,
                        returns_borrow,
                        returns_inout,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::FunctionDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        let params = source
                            .params(params)
                            .values()
                            .enumerate()
                            .map(|(parameter, param)| {
                                Ok(RirParam {
                                    name: symbol(param.name),
                                    ty: remap_type(param.ty),
                                    span: take_span(RirSpanField::FunctionParameter {
                                        parameter: u32::try_from(parameter)
                                            .expect("validated parameter count is encoded as u32"),
                                    })?,
                                    ..param
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_fn_decl_with_return_modes(
                            &directives,
                            *is_pub,
                            *is_unchecked,
                            *is_extern,
                            *is_c_export,
                            symbol(*name),
                            &params,
                            remap_type(*return_type),
                            remap_ref(*body),
                            *has_self,
                            *self_mode,
                            *self_is_mut,
                            *returns_borrow,
                            *returns_inout,
                            span,
                        )?
                    }
                    InstData::ConstDecl {
                        directives,
                        is_pub,
                        name,
                        ty,
                        init,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::ConstDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_const_decl(
                            &directives,
                            *is_pub,
                            symbol(*name),
                            ty.map(remap_type),
                            remap_ref(*init),
                            span,
                        )?
                    }
                    InstData::Call { name, args } => {
                        let args = remap_call_args(source.call_args(args), remap_ref);
                        self.add_call(symbol(*name), &args, span)?
                    }
                    InstData::Intrinsic { name, args } => {
                        let args = source
                            .intrinsic_args(args)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_intrinsic(symbol(*name), &args, span)?
                    }
                    InstData::InternalIntrinsic { intrinsic, args } => {
                        let args = source
                            .internal_intrinsic_args(args)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_internal_intrinsic(*intrinsic, &args, span)?
                    }
                    InstData::TypeIntrinsic { name, type_arg } => {
                        self.add_inst(payload_free(InstData::TypeIntrinsic {
                            name: symbol(*name),
                            type_arg: remap_type(*type_arg),
                        }))
                    }
                    InstData::OffsetOf { type_arg, field } => {
                        self.add_inst(payload_free(InstData::OffsetOf {
                            type_arg: remap_type(*type_arg),
                            field: symbol(*field),
                        }))
                    }
                    InstData::Ret(value) => {
                        self.add_inst(payload_free(InstData::Ret(value.map(remap_ref))))
                    }
                    InstData::Yield(value) => {
                        self.add_inst(payload_free(InstData::Yield(remap_ref(*value))))
                    }
                    InstData::Block { instructions } => {
                        let instructions = source
                            .block_insts(instructions)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_block(&instructions, span)?
                    }
                    InstData::Alloc {
                        directives,
                        name,
                        is_mut,
                        ty,
                        init,
                        iter_elem,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::AllocDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_alloc(
                            &directives,
                            name.map(&mut symbol),
                            *is_mut,
                            ty.map(remap_type),
                            remap_ref(*init),
                            *iter_elem,
                            span,
                        )?
                    }
                    InstData::VarRef { name, anchor } => {
                        self.add_inst(payload_free(InstData::VarRef {
                            name: symbol(*name),
                            anchor: anchor.clone(),
                        }))
                    }
                    InstData::Assign { name, value } => {
                        self.add_inst(payload_free(InstData::Assign {
                            name: symbol(*name),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::PlaceSet { place, value } => {
                        self.add_inst(payload_free(InstData::PlaceSet {
                            place: remap_ref(*place),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::StructDecl {
                        directives,
                        is_pub,
                        is_linear,
                        name,
                        fields,
                        methods,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::StructDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        let fields = source
                            .struct_fields(fields)
                            .values()
                            .map(|(name, ty)| (symbol(name), remap_type(ty)))
                            .collect::<Vec<_>>();
                        let methods = struct_methods_override
                            .as_ref()
                            .filter(|override_| override_.source_root == source_instruction)
                            .map_or_else(
                                || {
                                    source
                                        .struct_methods(methods)
                                        .values()
                                        .map(remap_ref)
                                        .collect::<Vec<_>>()
                                },
                                |override_| override_.destination_methods.to_vec(),
                            );
                        self.add_struct_decl(
                            &directives,
                            *is_pub,
                            *is_linear,
                            symbol(*name),
                            &fields,
                            &methods,
                            span,
                        )?
                    }
                    InstData::StructInit {
                        module,
                        ctor_head,
                        type_name,
                        fields,
                        shorthand_span,
                    } => {
                        let fields = source
                            .field_inits(fields)
                            .values()
                            .map(|(name, value)| (symbol(name), remap_ref(value)))
                            .collect::<Vec<_>>();
                        self.add_struct_init(
                            module.map(remap_ref),
                            ctor_head.map(remap_ref),
                            symbol(*type_name),
                            &fields,
                            shorthand_span
                                .map(|_| take_span(RirSpanField::StructInitShorthand))
                                .transpose()?,
                            span,
                        )?
                    }
                    InstData::FieldGet { base, field } => {
                        self.add_inst(payload_free(InstData::FieldGet {
                            base: remap_ref(*base),
                            field: symbol(*field),
                        }))
                    }
                    InstData::FieldSet { base, field, value } => {
                        self.add_inst(payload_free(InstData::FieldSet {
                            base: remap_ref(*base),
                            field: symbol(*field),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::EnumDecl {
                        is_pub,
                        is_non_exhaustive,
                        name,
                        variants: variant_range,
                        payloads,
                    } => {
                        let variants = source
                            .enum_variants(variant_range)
                            .values()
                            .map(&mut symbol)
                            .collect::<Vec<_>>();
                        let payloads = source
                            .enum_payloads(payloads, variant_range)
                            .map(|payload| payload.values().map(remap_type).collect())
                            .collect::<Vec<Vec<_>>>();
                        self.add_enum_decl(
                            *is_pub,
                            *is_non_exhaustive,
                            symbol(*name),
                            &variants,
                            &payloads,
                            span,
                        )?
                    }
                    InstData::EnumVariant {
                        module,
                        type_name,
                        variant,
                    } => self.add_inst(payload_free(InstData::EnumVariant {
                        module: module.map(remap_ref),
                        type_name: symbol(*type_name),
                        variant: symbol(*variant),
                    })),
                    InstData::ArrayInit { elements } => {
                        let elements = source
                            .array_elements(elements)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_array_init(&elements, span)?
                    }
                    InstData::ArrayRepeat { value, count } => {
                        self.add_inst(payload_free(InstData::ArrayRepeat {
                            value: remap_ref(*value),
                            count: match count {
                                RepeatCount::Literal(value) => RepeatCount::Literal(*value),
                                RepeatCount::Named(name) => RepeatCount::Named(symbol(*name)),
                            },
                        }))
                    }
                    InstData::IndexGet { base, index } => {
                        self.add_inst(payload_free(InstData::IndexGet {
                            base: remap_ref(*base),
                            index: remap_ref(*index),
                        }))
                    }
                    InstData::IndexSet { base, index, value } => {
                        self.add_inst(payload_free(InstData::IndexSet {
                            base: remap_ref(*base),
                            index: remap_ref(*index),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::MethodCall {
                        receiver,
                        method,
                        args,
                    } => {
                        let args = remap_call_args(source.call_args(args), remap_ref);
                        self.add_method_call(remap_ref(*receiver), symbol(*method), &args, span)?
                    }
                    InstData::DropFnDecl { type_name, body } => {
                        self.add_inst(payload_free(InstData::DropFnDecl {
                            type_name: symbol(*type_name),
                            body: remap_ref(*body),
                        }))
                    }
                    InstData::Comptime { expr } => {
                        self.add_inst(payload_free(InstData::Comptime {
                            expr: remap_ref(*expr),
                        }))
                    }
                    InstData::Checked { expr } => self.add_inst(payload_free(InstData::Checked {
                        expr: remap_ref(*expr),
                    })),
                    InstData::TypeConst { type_name } => {
                        self.add_inst(payload_free(InstData::TypeConst {
                            type_name: remap_type(*type_name),
                        }))
                    }
                    InstData::AnonStructType {
                        fields,
                        methods,
                        anchor,
                    } => {
                        let fields = source
                            .anon_struct_fields(fields)
                            .values()
                            .map(|(name, ty)| (symbol(name), remap_type(ty)))
                            .collect::<Vec<_>>();
                        let methods = source
                            .anon_struct_methods(methods)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_anon_struct_type(&fields, &methods, anchor.clone(), span)?
                    }
                    InstData::AnonEnumType {
                        variants: variant_range,
                        payloads,
                        anchor,
                    } => {
                        let variants = source
                            .anon_enum_variants(variant_range)
                            .values()
                            .map(&mut symbol)
                            .collect::<Vec<_>>();
                        let payloads = source
                            .anon_enum_payloads(payloads, variant_range)
                            .map(|payload| payload.values().map(remap_type).collect())
                            .collect::<Vec<Vec<_>>>();
                        self.add_anon_enum_type(&variants, &payloads, anchor.clone(), span)?
                    }
                };
            }
            if let Some((slot, _)) = mapped_spans.next() {
                return Err(RirSpanRemapError::UnconsumedSlot(slot));
            }
            if let Some(error) = self.rir.latched_capacity_error() {
                return Err(RirSpanRemapError::Build(error));
            }
            let instruction_end = u32::try_from(self.rir.instructions.len()).map_err(|_| {
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: "instructions",
                }
            })?;
            let extra_end = u32::try_from(self.rir.extra.len()).map_err(|_| {
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: "payload words",
                }
            })?;
            Ok(RirAppendRange {
                instructions: instruction_start..instruction_end,
                extra: extra_start..extra_end,
            })
        })();
        if result.is_err() {
            self.rir.instructions.truncate(instruction_start as usize);
            self.rir.extra.truncate(extra_start as usize);
            self.type_syntax.rollback(type_snapshot);
        }
        result
    }

    /// Atomically replace an instruction with a compiler-internal intrinsic.
    pub fn replace_internal_intrinsic(
        &mut self,
        instruction: InstRef,
        intrinsic: InternalIntrinsic,
        args: &[InstRef],
    ) -> Result<(), RirPayloadBuildError> {
        if self
            .rir
            .instructions
            .get(instruction.as_u32() as usize)
            .is_none()
        {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: RirInternalIntrinsicArgsRange::FAMILY,
                reason: "replacement instruction is outside the editor",
            });
        }
        let range = self.rir.add_internal_intrinsic_args(args)?;
        let inst = &mut self.rir.instructions[instruction.as_u32() as usize];
        inst.data = InstData::InternalIntrinsic {
            intrinsic,
            args: range,
        };
        Ok(())
    }

    /// Change function visibility without exposing detached instruction data.
    pub fn set_function_public(
        &mut self,
        instruction: InstRef,
        is_pub: bool,
    ) -> Result<(), RirPayloadBuildError> {
        let Some(inst) = self.rir.instructions.get_mut(instruction.as_u32() as usize) else {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: "function declaration",
                reason: "replacement instruction is outside the editor",
            });
        };
        let InstData::FnDecl { is_pub: slot, .. } = &mut inst.data else {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: "function declaration",
                reason: "visibility replacement requires a function declaration",
            });
        };
        *slot = is_pub;
        Ok(())
    }
}

impl std::ops::Deref for RirEditor {
    type Target = Rir;

    fn deref(&self) -> &Self::Target {
        &self.rir
    }
}
