//! Canonical span-slot schema, traversal, and in-place rewriting.

use super::*;

#[path = "validation.rs"]
mod validation;

pub use validation::*;

/// Stable tag for a span-bearing field inside one RIR instruction.
///
/// Record indices are local to their typed payload. Optional fields have their
/// own tags, so adding or removing one never renumbers a later instruction or
/// a different field family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RirSpanField {
    Instruction,
    MatchPattern { arm: u32 },
    FunctionDirective { directive: u32 },
    FunctionParameter { parameter: u32 },
    ConstDirective { directive: u32 },
    AllocDirective { directive: u32 },
    StructDirective { directive: u32 },
    StructInitShorthand,
}

/// Position-independent identity of one span field in structurally equal RIR.
///
/// The instruction index is a dense structural RIR location. It is never
/// derived from callback order, source coordinates, tokens, or spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirSpanSlot {
    instruction: InstRef,
    field: RirSpanField,
}

impl RirSpanSlot {
    const fn new(instruction: InstRef, field: RirSpanField) -> Self {
        Self { instruction, field }
    }

    pub const fn instruction(self) -> InstRef {
        self.instruction
    }

    pub const fn field(self) -> RirSpanField {
        self.field
    }
}

/// Stable inventory of the span-bearing storage families in RIR.
///
/// `api_inventory` ties this list to the concrete storage declarations and to
/// the canonical visitor. Adding a span field therefore requires an explicit
/// visitor/schema update.
pub const RIR_SPAN_FIELD_FAMILY_NAMES: [&str; 5] = [
    "instruction",
    "directive",
    "parameter",
    "match pattern",
    "struct-init shorthand",
];

/// Failure from canonical RIR span-slot traversal.
#[derive(Debug)]
pub enum RirSpanTraversalError<E> {
    MalformedPayload(RirPayloadError),
    DuplicateSlot(RirSpanSlot),
    Callback(E),
}

/// Failure while atomically appending and remapping a RIR owner by span slot.
#[derive(Debug)]
pub enum RirSpanRemapError<E> {
    MalformedPayload(RirPayloadError),
    MalformedTypeSyntax(crate::RirTypeSyntaxValidationError),
    DuplicateSlot(RirSpanSlot),
    MissingSlot(RirSpanSlot),
    UnexpectedSlot {
        expected: RirSpanSlot,
        actual: RirSpanSlot,
    },
    UnconsumedSlot(RirSpanSlot),
    InvalidInstructionRange(std::ops::Range<u32>),
    ForeignInstructionEdge {
        instruction: InstRef,
        child: InstRef,
    },
    Checkpoint(E),
    Mapping {
        slot: RirSpanSlot,
        error: E,
    },
    Build(RirPayloadBuildError),
}

impl<E> From<RirPayloadBuildError> for RirSpanRemapError<E> {
    fn from(error: RirPayloadBuildError) -> Self {
        Self::Build(error)
    }
}

impl Rir {
    /// Validate payload structure, then visit every canonical span slot.
    /// Published owners should use [`ValidatedRir::try_visit_span_slots`] to
    /// avoid repeating validation.
    pub fn try_visit_span_slots<E>(
        &self,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        checkpoint().map_err(RirSpanTraversalError::Callback)?;
        self.validate_payloads()
            .map_err(RirSpanTraversalError::MalformedPayload)?;
        self.try_visit_validated_span_slots(checkpoint, visit)
    }

    fn try_visit_validated_span_slots<E>(
        &self,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.try_visit_validated_instruction_range_span_slots(
            0..u32::try_from(self.len()).unwrap_or(u32::MAX),
            checkpoint,
            visit,
        )
    }

    fn try_visit_validated_instruction_range_span_slots<E>(
        &self,
        instructions: std::ops::Range<u32>,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        let mut previous_slot = None;

        for ordinal in instructions {
            let instruction = InstRef::from_raw(ordinal);
            let inst = self.get(instruction);
            checkpoint().map_err(RirSpanTraversalError::Callback)?;

            macro_rules! emit {
                ($field:expr, $span:expr) => {{
                    let slot = RirSpanSlot::new(instruction, $field);
                    if previous_slot.is_some_and(|previous| previous >= slot) {
                        return Err(RirSpanTraversalError::DuplicateSlot(slot));
                    }
                    previous_slot = Some(slot);
                    visit(slot, $span).map_err(RirSpanTraversalError::Callback)?;
                }};
            }

            emit!(RirSpanField::Instruction, inst.span);
            match &inst.data {
                InstData::Match { arms, .. } => {
                    for (arm, (pattern, _)) in self.match_arms(arms).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::MatchPattern {
                                arm: u32::try_from(arm)
                                    .expect("validated match-arm count is encoded as u32"),
                            },
                            pattern.span()
                        );
                    }
                }
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::FunctionDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                    for (parameter, value) in self.params(params).values().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::FunctionParameter {
                                parameter: u32::try_from(parameter)
                                    .expect("validated parameter count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::ConstDecl { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::ConstDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::Alloc { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::AllocDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::StructDecl { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::StructDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::StructInit {
                    shorthand_span: Some(span),
                    ..
                } => emit!(RirSpanField::StructInitShorthand, *span),
                _ => {}
            }
        }
        Ok(())
    }

    fn try_rewrite_validated_span_slots<E>(
        &mut self,
        mapped_spans: &[(RirSpanSlot, Span)],
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(), RirSpanRemapError<E>> {
        let mut mapped_spans = mapped_spans.iter().copied();
        let mut take_span = |expected| {
            let Some((actual, span)) = mapped_spans.next() else {
                return Err(RirSpanRemapError::MissingSlot(expected));
            };
            if actual != expected {
                return Err(RirSpanRemapError::UnexpectedSlot { expected, actual });
            }
            Ok(span)
        };
        let (instructions, extra) = (&mut self.instructions, &mut self.extra);

        for (ordinal, instruction) in instructions.iter_mut().enumerate() {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let instruction_ref = InstRef::from_raw(
                u32::try_from(ordinal).expect("validated RIR instruction index fits u32"),
            );
            instruction.span =
                take_span(RirSpanSlot::new(instruction_ref, RirSpanField::Instruction))?;

            let mut rewrite_directives = |range: &RirDirectivesRange,
                                          field: &mut dyn FnMut(u32) -> RirSpanField|
             -> Result<(), RirSpanRemapError<E>> {
                let start = range.start() as usize;
                let end = start + range.extent() as usize;
                let words = &mut extra[start..end];
                if words.is_empty() {
                    return Ok(());
                }
                let count = words[0] as usize;
                let mut position = 1usize;
                for directive in 0..count {
                    checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                    let extent = decoded_directive_record_extent(words, position)
                        .expect("validated directive record has an exact extent");
                    let span = take_span(RirSpanSlot::new(
                        instruction_ref,
                        field(
                            u32::try_from(directive)
                                .expect("validated directive count is encoded as u32"),
                        ),
                    ))?;
                    words[position + RECORD_SPAN_START] = span.start;
                    words[position + RECORD_SPAN_LEN] = span.end - span.start;
                    words[position + RECORD_SPAN_FILE] = span.file_id.index();
                    position += extent;
                }
                Ok(())
            };

            match &mut instruction.data {
                InstData::Match { arms, .. } => {
                    let start = arms.start() as usize;
                    let end = start + arms.extent() as usize;
                    let words = &mut extra[start..end];
                    if !words.is_empty() {
                        let count = words[0] as usize;
                        let mut position = 1usize;
                        for arm in 0..count {
                            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                            let extent = decoded_match_record_extent(words, position)
                                .expect("validated match record has an exact extent");
                            let span = take_span(RirSpanSlot::new(
                                instruction_ref,
                                RirSpanField::MatchPattern {
                                    arm: u32::try_from(arm)
                                        .expect("validated match-arm count is encoded as u32"),
                                },
                            ))?;
                            words[position + RECORD_SPAN_START] = span.start;
                            words[position + RECORD_SPAN_LEN] = span.end - span.start;
                            words[position + RECORD_SPAN_FILE] = span.file_id.index();
                            position += extent;
                        }
                    }
                }
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::FunctionDirective { directive }
                    })?;
                    let start = params.start() as usize;
                    let end = start + params.extent() as usize;
                    for (parameter, words) in extra[start..end]
                        .chunks_exact_mut(PARAM_SCHEMA.width)
                        .enumerate()
                    {
                        checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                        let span = take_span(RirSpanSlot::new(
                            instruction_ref,
                            RirSpanField::FunctionParameter {
                                parameter: u32::try_from(parameter)
                                    .expect("validated parameter count is encoded as u32"),
                            },
                        ))?;
                        words[PARAM_SPAN_FILE] = span.file_id.index();
                        words[PARAM_SPAN_START] = span.start;
                        words[PARAM_SPAN_END] = span.end;
                    }
                }
                InstData::ConstDecl { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::ConstDirective { directive }
                    })?;
                }
                InstData::Alloc { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::AllocDirective { directive }
                    })?;
                }
                InstData::StructDecl { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::StructDirective { directive }
                    })?;
                }
                InstData::StructInit {
                    shorthand_span: Some(span),
                    ..
                } => {
                    *span = take_span(RirSpanSlot::new(
                        instruction_ref,
                        RirSpanField::StructInitShorthand,
                    ))?;
                }
                _ => {}
            }
        }

        if let Some((slot, _)) = mapped_spans.next() {
            return Err(RirSpanRemapError::UnconsumedSlot(slot));
        }
        Ok(())
    }
}
