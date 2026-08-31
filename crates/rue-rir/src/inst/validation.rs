//! Immutable publication and structural graph validation.

use super::*;

#[path = "editor.rs"]
mod editor;
#[path = "payload_support.rs"]
mod payload_support;

pub use editor::*;

/// Canonical source/interner bounds required to publish an immutable RIR.
pub struct RirValidationContext<'a> {
    pub symbol_count: usize,
    pub source_lengths: &'a [(FileId, u32)],
}

/// Immutable RIR whose complete payload graph passed structural validation.
#[derive(Debug)]
pub struct ValidatedRir(Rir);

impl ValidatedRir {
    /// Publish an owner whose complete graph was validated by one of the two
    /// canonical validation paths: ordinary contextual validation below or
    /// the exact packed-envelope decoder.
    fn from_prevalidated(mut rir: Rir) -> Self {
        rir.views_validated = true;
        Self(rir)
    }

    /// Consume and validate an editor at the construction/publication boundary.
    pub fn finish(
        editor: RirEditor,
        context: &RirValidationContext<'_>,
    ) -> Result<Self, RirPayloadError> {
        // Structured type syntax is constructed in the editor-owned builder
        // and installed into the immutable RIR only at publication. Validate
        // the published owner, not the editor's still-empty frozen field.
        let rir = editor.into_unvalidated();
        rir.validate_payloads()?;
        rir.validate_context(context)?;
        Ok(Self::from_prevalidated(rir))
    }

    /// Visit every span-bearing RIR slot through the canonical schema.
    ///
    /// `checkpoint` is called at instruction and payload-record granularity,
    /// allowing cancellation before a large owner is fully traversed.
    pub fn try_visit_span_slots<E>(
        &self,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.0.try_visit_validated_span_slots(checkpoint, visit)
    }

    /// Consume this validated owner and rewrite every canonical span slot in
    /// place, preserving the instruction and payload-word allocations.
    ///
    /// Mapping completes before the first write, so a callback failure cannot
    /// leave a partially rewritten owner observable. The rewritten owner is
    /// validated against `context` before publication. Instruction spans,
    /// match-pattern spans, directives, parameters, and struct-initializer
    /// shorthand spans all pass through the same canonical slot schema used by
    /// [`Self::try_visit_span_slots`].
    pub fn try_rewrite_span_slots<E>(
        mut self,
        context: &RirValidationContext<'_>,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<Self, RirSpanRemapError<E>> {
        enum CollectError<E> {
            Checkpoint(E),
            Mapping { slot: RirSpanSlot, error: E },
        }

        let mut mapped_spans = Vec::new();
        let traversal = self.try_visit_span_slots(
            || checkpoint().map_err(CollectError::Checkpoint),
            |slot, span| {
                let mapped = remap_span(slot, span)
                    .map_err(|error| CollectError::Mapping { slot, error })?;
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

        for (slot, span) in &mapped_spans {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let reason = match context
                .source_lengths
                .iter()
                .find(|(file, _)| *file == span.file_id)
            {
                None => Some("span file is outside the canonical source revision"),
                Some((_, source_len)) if span.start > span.end || span.end > *source_len => {
                    Some("span range is outside its canonical source")
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                return Err(RirSpanRemapError::MalformedPayload(RirPayloadError::new(
                    "instruction context",
                    slot.instruction().as_u32(),
                    1,
                    None,
                    1,
                    1,
                    reason,
                )));
            }
        }

        self.0
            .try_rewrite_validated_span_slots(&mapped_spans, &mut checkpoint)?;
        self.0
            .validate_payloads()
            .map_err(RirSpanRemapError::MalformedPayload)?;
        self.0
            .validate_context(context)
            .map_err(RirSpanRemapError::MalformedPayload)?;
        Ok(self)
    }

    /// Visit the canonical span schema for one prevalidated contiguous
    /// declaration-producer interval.
    #[doc(hidden)]
    fn try_visit_instruction_range_span_slots<E>(
        &self,
        instructions: std::ops::Range<u32>,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.0
            .try_visit_validated_instruction_range_span_slots(instructions, checkpoint, visit)
    }

    /// Exact equality of the validated dense representation. Candidate body
    /// plans zero every positional span under the reserved structural FileId;
    /// their ordered declaration-relative diagnostic basis is compared by the
    /// owning artifact terminal.
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    /// Logical heap bytes retained by this RIR owner, excluding the inline
    /// [`ValidatedRir`] value itself.
    ///
    /// Dense instruction and payload storage is charged by logical length.
    /// Every structural-anchor `Arc` pointee is charged in full along each
    /// reaching instruction path, including when multiple instructions share
    /// one allocation. This matches Rue's allocator-independent retained-value
    /// policy and leaves the enclosing owner responsible for the inline value.
    pub fn retained_allocation_charge(&self) -> u64 {
        let instructions = self.len().saturating_mul(std::mem::size_of::<Inst>()) as u64;
        let payload = self.extra_len().saturating_mul(std::mem::size_of::<u32>()) as u64;
        let type_syntax = self.type_syntax().retained_allocation_charge();
        self.iter().fold(
            instructions
                .saturating_add(payload)
                .saturating_add(type_syntax),
            |charge, (_, instruction)| {
                let anchors = match &instruction.data {
                    InstData::StringConst { anchor, .. }
                    | InstData::AnonStructType { anchor, .. }
                    | InstData::AnonEnumType { anchor, .. } => {
                        std::mem::size_of_val(anchor.segments()) as u64
                    }
                    InstData::VarRef {
                        anchor: Some(anchor),
                        ..
                    } => std::mem::size_of_val(anchor.segments()) as u64,
                    _ => 0,
                };
                charge.saturating_add(anchors)
            },
        )
    }
}

impl std::ops::Deref for ValidatedRir {
    type Target = Rir;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Rir {
    fn validate_fixed<R>(
        &self,
        range: &R,
        width: usize,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.len() % width != 0 {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some((words.len() / width) as u32),
                expected: width,
                actual: words.len() % width,
                reason: "payload ends in a partial record",
            });
        }
        Ok(())
    }

    fn validate_fixed_symbols<R>(
        &self,
        range: &R,
        schema: FixedPayloadSchema,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        self.validate_fixed(range, schema.width, |_| (start, extent, family))?;
        for (record, words) in self
            .payload_words(range, |_| (start, extent, family))?
            .chunks_exact(schema.width)
            .enumerate()
        {
            if schema
                .symbol_offsets
                .iter()
                .any(|offset| decode_symbol_word(words[*offset]).is_none())
            {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: schema.width,
                    actual: schema.width,
                    reason: "symbol word is not representable",
                });
            }
        }
        Ok(())
    }

    fn validate_variable_records<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
        record_extent: impl Fn(&[u32], usize) -> Option<usize>,
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.is_empty() {
            return Ok(());
        }
        let count = words[0] as usize;
        let mut pos = 1usize;
        for record in 0..count {
            let Some(width) = record_extent(words, pos) else {
                let remaining = words.len().saturating_sub(pos);
                let (expected, reason) = if family == RirMatchArmsRange::FAMILY {
                    match words.get(pos + RECORD_KIND).copied() {
                        None => (RECORD_KIND + 1, "record header is truncated"),
                        Some(kind) if kind == PatternKind::Path as u32 => (
                            MATCH_PATH_BINDING_COUNT + 1,
                            "path record header is truncated",
                        ),
                        Some(kind)
                            if kind != PatternKind::Wildcard as u32
                                && kind != PatternKind::Int as u32
                                && kind != PatternKind::Bool as u32 =>
                        {
                            (1, "invalid pattern kind")
                        }
                        Some(_) => (1, "record extent is not representable"),
                    }
                } else {
                    (
                        DIRECTIVE_ARG_COUNT + 1,
                        "directive record header is truncated",
                    )
                };
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: expected,
                    actual: remaining.min(expected),
                    reason: reason,
                });
            };
            pos = pos.checked_add(width).ok_or_else(|| {
                rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: width,
                    actual: words.len().saturating_sub(pos),
                    reason: "record end overflows usize",
                }
            })?;
            if pos > words.len() {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: width,
                    actual: words.len().saturating_sub(pos - width),
                    reason: "record body is truncated",
                });
            }
        }
        if pos != words.len() {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some(count as u32),
                expected: 0,
                actual: words.len().saturating_sub(pos),
                reason: "trailing words after final record",
            });
        }
        Ok(())
    }

    /// Validate every variable-length payload before publishing this RIR.
    pub fn validate_payloads(&self) -> Result<(), RirPayloadError> {
        for (_, inst) in self.iter() {
            match &inst.data {
                InstData::Match { arms, .. } => self.validate_match_range(arms)?,
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    self.validate_directive_range(directives)?;
                    self.validate_fixed_symbols(params, PARAM_SCHEMA, |r| {
                        (r.start(), r.extent(), RirParamsRange::FAMILY)
                    })?;
                    for (record, words) in self
                        .payload_words(params, |r| (r.start(), r.extent(), RirParamsRange::FAMILY))?
                        .chunks_exact(PARAM_SCHEMA.width)
                        .enumerate()
                    {
                        if words[PARAM_MODE] > RirParamMode::Borrow as u32 {
                            return Err(rir_payload_error! {
                                family: RirParamsRange::FAMILY,
                                start: params.start(),
                                extent: params.extent(),
                                record: Some(record as u32),
                                expected: PARAM_SCHEMA.width,
                                actual: PARAM_SCHEMA.width,
                                reason: "invalid parameter mode",
                            });
                        }
                        if words[PARAM_COMPTIME] > 1 {
                            return Err(rir_payload_error! {
                                family: RirParamsRange::FAMILY,
                                start: params.start(),
                                extent: params.extent(),
                                record: Some(record as u32),
                                expected: PARAM_SCHEMA.width,
                                actual: PARAM_SCHEMA.width,
                                reason: "invalid comptime flag",
                            });
                        }
                    }
                }
                InstData::ConstDecl { directives, .. } | InstData::Alloc { directives, .. } => {
                    self.validate_directive_range(directives)?
                }
                InstData::Call { args, .. } | InstData::MethodCall { args, .. } => {
                    self.validate_fixed(args, CALL_ARG_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirCallArgsRange::FAMILY)
                    })?;
                    for (record, words) in self
                        .payload_words(args, |r| (r.start(), r.extent(), RirCallArgsRange::FAMILY))?
                        .chunks_exact(CALL_ARG_SCHEMA.width)
                        .enumerate()
                    {
                        if words[CALL_ARG_MODE] > RirArgMode::Borrow as u32 {
                            return Err(rir_payload_error! {
                                family: RirCallArgsRange::FAMILY,
                                start: args.start(),
                                extent: args.extent(),
                                record: Some(record as u32),
                                expected: CALL_ARG_SCHEMA.width,
                                actual: CALL_ARG_SCHEMA.width,
                                reason: "invalid argument mode",
                            });
                        }
                    }
                }
                InstData::Intrinsic { args, .. } => {
                    self.validate_fixed(args, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirIntrinsicArgsRange::FAMILY)
                    })?
                }
                InstData::InternalIntrinsic { args, .. } => {
                    self.validate_fixed(args, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirInternalIntrinsicArgsRange::FAMILY)
                    })?
                }
                InstData::Block { instructions } => {
                    self.validate_fixed(instructions, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirBlockInstsRange::FAMILY)
                    })?
                }
                InstData::StructDecl {
                    directives,
                    fields,
                    methods,
                    ..
                } => {
                    self.validate_directive_range(directives)?;
                    self.validate_fixed_symbols(fields, FIELD_DECL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirStructFieldsRange::FAMILY)
                    })?;
                    self.validate_fixed(methods, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirStructMethodsRange::FAMILY)
                    })?;
                }
                InstData::StructInit { fields, .. } => {
                    self.validate_fixed_symbols(fields, FIELD_INIT_SCHEMA, |r| {
                        (r.start(), r.extent(), RirFieldInitsRange::FAMILY)
                    })?
                }
                InstData::EnumDecl {
                    variants, payloads, ..
                } => {
                    self.validate_fixed_symbols(variants, SYMBOL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirEnumVariantsRange::FAMILY)
                    })?;
                    self.validate_enum_payload_range(payloads, variants.extent() as usize)?;
                }
                InstData::ArrayInit { elements } => {
                    self.validate_fixed(elements, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirArrayElemsRange::FAMILY)
                    })?
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    self.validate_fixed_symbols(fields, FIELD_DECL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirAnonStructFieldsRange::FAMILY)
                    })?;
                    self.validate_fixed(methods, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirAnonStructMethodsRange::FAMILY)
                    })?;
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    self.validate_fixed_symbols(variants, SYMBOL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirAnonEnumVariantsRange::FAMILY)
                    })?;
                    self.validate_anon_enum_payload_range(payloads, variants.extent() as usize)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_match_range(&self, range: &RirMatchArmsRange) -> Result<(), RirPayloadError> {
        self.validate_variable_records(
            range,
            |r| (r.start(), r.extent(), RirMatchArmsRange::FAMILY),
            decoded_match_record_extent,
        )?;
        let words = self.payload_words(range, |r| {
            (r.start(), r.extent(), RirMatchArmsRange::FAMILY)
        })?;
        if words.is_empty() {
            return Ok(());
        }
        let mut position = 1usize;
        for record in 0..words[0] as usize {
            let record_width = decoded_match_record_extent(words, position)
                .expect("variable-record validation established match extent");
            let kind = words[position + RECORD_KIND];
            if embedded_span(words, position).is_none() {
                return Err(rir_payload_error! {
                    family: RirMatchArmsRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "pattern span overflows u32",
                });
            }
            if kind == PatternKind::Int as u32 {
                if words[position + MATCH_INT_NEGATIVE_OR_PATH_TYPE] > 1 {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "invalid integer-sign flag",
                    });
                }
            } else if kind == PatternKind::Bool as u32 {
                if words[position + MATCH_VALUE_LO_OR_BOOL_OR_BODY] > 1 {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "invalid boolean scalar",
                    });
                }
            } else if kind == PatternKind::Path as u32 {
                let binding_count = words[position + MATCH_PATH_BINDING_COUNT] as usize;
                let binding_start = position + MATCH_PATH_BINDINGS_START;
                let binding_end = binding_start + binding_count;
                if words[binding_start..binding_end]
                    .iter()
                    .any(|word| decode_symbol_word(*word).is_none())
                {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "symbol word is not representable",
                    });
                }
            }
            let (_, _, width) = decode_match_record(words, position, false).ok_or_else(|| {
                rir_payload_error! {
                    family: RirMatchArmsRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "match record failed schema decoding",
                }
            })?;
            position += width;
        }
        Ok(())
    }

    /// Validate every context-dependent handle after structural payload
    /// validation and before any infallible borrowing view is published.
    pub fn validate_context(
        &self,
        context: &RirValidationContext<'_>,
    ) -> Result<(), RirPayloadError> {
        fn error(index: u32, reason: &'static str) -> RirPayloadError {
            rir_payload_error! {
                family: "instruction context",
                start: index,
                extent: 1,
                record: None,
                expected: 1,
                actual: 1,
                reason,
            }
        }
        let check_ref = |index: u32, reference: InstRef| {
            if (reference.as_u32() as usize) < self.instructions.len() {
                Ok(())
            } else {
                Err(error(index, "instruction reference is outside the owner"))
            }
        };
        let check_symbol = |index: u32, symbol: Spur| {
            if symbol.into_usize() < context.symbol_count {
                Ok(())
            } else {
                Err(error(index, "symbol is outside the canonical interner"))
            }
        };
        let check_span = |index: u32, span: Span| {
            let Some((_, source_len)) = context
                .source_lengths
                .iter()
                .find(|(file, _)| *file == span.file_id)
            else {
                return Err(error(
                    index,
                    "span file is outside the canonical source revision",
                ));
            };
            if span.start <= span.end && span.end <= *source_len {
                Ok(())
            } else {
                Err(error(index, "span range is outside its canonical source"))
            }
        };

        self.type_syntax
            .validate_with_symbol(|symbol| symbol.into_usize() < context.symbol_count)
            .map_err(|failure| {
                error(
                    failure.node.map_or(u32::MAX, RirTypeSyntaxRef::as_u32),
                    failure.reason,
                )
            })?;

        for (instruction, inst) in self.iter() {
            let index = instruction.as_u32();
            check_span(index, inst.span)?;
            macro_rules! refs {
                ($($reference:expr),* $(,)?) => {{ $(check_ref(index, $reference)?;)* }};
            }
            macro_rules! symbols {
                ($($symbol:expr),* $(,)?) => {{ $(check_symbol(index, $symbol)?;)* }};
            }
            macro_rules! types {
                ($($reference:expr),* $(,)?) => {{
                    $(if $reference.index() >= self.type_syntax.nodes().len() {
                        return Err(error(index, "type-syntax reference is outside the owner"));
                    })*
                }};
            }
            match &inst.data {
                InstData::IntConst(_)
                | InstData::BoolConst(_)
                | InstData::UnitConst
                | InstData::Continue => {}
                InstData::StringConst {
                    content: symbol, ..
                }
                | InstData::FloatConst { text: symbol }
                | InstData::VarRef { name: symbol, .. } => symbols!(*symbol),
                InstData::TypeConst { type_name } => types!(*type_name),
                InstData::Add { lhs, rhs }
                | InstData::Sub { lhs, rhs }
                | InstData::Mul { lhs, rhs }
                | InstData::Div { lhs, rhs }
                | InstData::Mod { lhs, rhs }
                | InstData::Eq { lhs, rhs }
                | InstData::Ne { lhs, rhs }
                | InstData::Lt { lhs, rhs }
                | InstData::Gt { lhs, rhs }
                | InstData::Le { lhs, rhs }
                | InstData::Ge { lhs, rhs }
                | InstData::And { lhs, rhs }
                | InstData::Or { lhs, rhs }
                | InstData::BitAnd { lhs, rhs }
                | InstData::BitOr { lhs, rhs }
                | InstData::BitXor { lhs, rhs }
                | InstData::Shl { lhs, rhs }
                | InstData::Shr { lhs, rhs } => refs!(*lhs, *rhs),
                InstData::Neg { operand }
                | InstData::Not { operand }
                | InstData::BitNot { operand }
                | InstData::Try { operand }
                | InstData::Comptime { expr: operand }
                | InstData::Checked { expr: operand } => refs!(*operand),
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    refs!(*cond, *then_block);
                    if let Some(reference) = else_block {
                        refs!(*reference);
                    }
                }
                InstData::Loop { cond, body } => refs!(*cond, *body),
                InstData::InfiniteLoop { body, iter_borrow } => {
                    refs!(*body);
                    if let Some(symbol) = iter_borrow {
                        symbols!(*symbol);
                    }
                }
                InstData::Match { scrutinee, arms } => {
                    refs!(*scrutinee);
                    for (pattern, body) in self.match_arms(arms).iter() {
                        refs!(body);
                        check_span(index, pattern.span())?;
                        if let RirPatternView::Path {
                            module,
                            ctor_head,
                            type_name,
                            variant,
                            bindings,
                            ..
                        } = pattern
                        {
                            if let Some(reference) = module {
                                refs!(reference);
                            }
                            if let Some(reference) = ctor_head {
                                refs!(reference);
                            }
                            symbols!(type_name, variant);
                            for binding in bindings {
                                symbols!(binding);
                            }
                        }
                    }
                }
                InstData::Break { value } | InstData::Ret(value) => {
                    if let Some(reference) = value {
                        refs!(*reference);
                    }
                }
                InstData::Yield(value) => refs!(*value),
                InstData::FnDecl {
                    directives,
                    name,
                    params,
                    return_type,
                    body,
                    ..
                } => {
                    symbols!(*name);
                    types!(*return_type);
                    refs!(*body);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                    for param in self.params(params) {
                        symbols!(param.name);
                        types!(param.ty);
                        check_span(index, param.span)?;
                    }
                }
                InstData::ConstDecl {
                    directives,
                    name,
                    ty,
                    init,
                    ..
                } => {
                    symbols!(*name);
                    if let Some(symbol) = ty {
                        types!(*symbol);
                    }
                    refs!(*init);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::Call { name, args } => {
                    symbols!(*name);
                    for arg in self.call_args(args) {
                        refs!(arg.value);
                    }
                }
                InstData::Intrinsic { name, args } => {
                    symbols!(*name);
                    for reference in self.intrinsic_args(args) {
                        refs!(reference);
                    }
                }
                InstData::InternalIntrinsic { args, .. } => {
                    for reference in self.internal_intrinsic_args(args) {
                        refs!(reference);
                    }
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    symbols!(*name);
                    types!(*type_arg);
                }
                InstData::OffsetOf { type_arg, field } => {
                    types!(*type_arg);
                    symbols!(*field);
                }
                InstData::Block { instructions } => {
                    for reference in self.block_insts(instructions) {
                        refs!(reference);
                    }
                }
                InstData::Alloc {
                    directives,
                    name,
                    ty,
                    init,
                    ..
                } => {
                    if let Some(symbol) = name {
                        symbols!(*symbol);
                    }
                    if let Some(symbol) = ty {
                        types!(*symbol);
                    }
                    refs!(*init);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::Assign { name, value } => {
                    symbols!(*name);
                    refs!(*value);
                }
                InstData::PlaceSet { place, value } => {
                    refs!(*place);
                    refs!(*value);
                }
                InstData::StructDecl {
                    directives,
                    name,
                    fields,
                    methods,
                    ..
                } => {
                    symbols!(*name);
                    for (field, ty) in self.struct_fields(fields) {
                        symbols!(field);
                        types!(ty);
                    }
                    for reference in self.struct_methods(methods) {
                        refs!(reference);
                    }
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span,
                } => {
                    if let Some(reference) = module {
                        refs!(*reference);
                    }
                    if let Some(reference) = ctor_head {
                        refs!(*reference);
                    }
                    symbols!(*type_name);
                    for (field, value) in self.field_inits(fields) {
                        symbols!(field);
                        refs!(value);
                    }
                    if let Some(span) = shorthand_span {
                        check_span(index, *span)?;
                    }
                }
                InstData::FieldGet { base, field } => {
                    refs!(*base);
                    symbols!(*field);
                }
                InstData::FieldSet { base, field, value } => {
                    refs!(*base, *value);
                    symbols!(*field);
                }
                InstData::EnumDecl {
                    name,
                    variants,
                    payloads,
                    ..
                } => {
                    symbols!(*name);
                    for variant in self.enum_variants(variants) {
                        symbols!(variant);
                    }
                    for payload in self.enum_payloads(payloads, variants) {
                        for ty in payload {
                            types!(ty);
                        }
                    }
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    if let Some(reference) = module {
                        refs!(*reference);
                    }
                    symbols!(*type_name, *variant);
                }
                InstData::ArrayInit { elements } => {
                    for reference in self.array_elements(elements) {
                        refs!(reference);
                    }
                }
                InstData::ArrayRepeat { value, count } => {
                    refs!(*value);
                    if let RepeatCount::Named(symbol) = count {
                        symbols!(*symbol);
                    }
                }
                InstData::IndexGet {
                    base,
                    index: subscript,
                } => refs!(*base, *subscript),
                InstData::IndexSet {
                    base,
                    index: subscript,
                    value,
                } => refs!(*base, *subscript, *value),
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    refs!(*receiver);
                    symbols!(*method);
                    for arg in self.call_args(args) {
                        refs!(arg.value);
                    }
                }
                InstData::DropFnDecl { type_name, body } => {
                    symbols!(*type_name);
                    refs!(*body);
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    for (field, ty) in self.anon_struct_fields(fields) {
                        symbols!(field);
                        types!(ty);
                    }
                    for reference in self.anon_struct_methods(methods) {
                        refs!(reference);
                    }
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    for variant in self.anon_enum_variants(variants) {
                        symbols!(variant);
                    }
                    for payload in self.anon_enum_payloads(payloads, variants) {
                        for ty in payload {
                            types!(ty);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Append every instruction `instruction` references — its operands, block
    /// members, match-arm bodies, call arguments, and nested declaration
    /// bodies — to `out`.
    ///
    /// The match is exhaustive with no catch-all arm, so a new [`InstData`]
    /// variant does not compile until its operands are listed here. That is
    /// what lets a consumer walk an instruction's whole subtree without
    /// silently missing a syntactic form; the accessor-body legality rules
    /// (spec 6.6:6, 6.6:7) decide containment questions this way, before any
    /// type resolves.
    ///
    /// Declaration-forming variants report their nested bodies as children. A
    /// consumer that must not cross a declaration boundary — a nested `fn` owns
    /// its own body — stops on the declaration instruction itself rather than
    /// filtering the children out here.
    pub fn child_instructions(&self, instruction: InstRef, out: &mut Vec<InstRef>) {
        match &self.get(instruction).data {
            InstData::IntConst(_)
            | InstData::FloatConst { .. }
            | InstData::BoolConst(_)
            | InstData::UnitConst
            | InstData::Continue
            | InstData::StringConst { .. }
            | InstData::VarRef { .. }
            | InstData::TypeConst { .. }
            | InstData::TypeIntrinsic { .. }
            | InstData::OffsetOf { .. }
            | InstData::EnumDecl { .. }
            | InstData::AnonEnumType { .. } => {}
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs }
            | InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs }
            | InstData::And { lhs, rhs }
            | InstData::Or { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => out.extend([*lhs, *rhs]),
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand }
            | InstData::Try { operand }
            | InstData::Comptime { expr: operand }
            | InstData::Checked { expr: operand }
            | InstData::Yield(operand) => out.push(*operand),
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                out.extend([*cond, *then_block]);
                out.extend(else_block.iter().copied());
            }
            InstData::Loop { cond, body } => out.extend([*cond, *body]),
            InstData::InfiniteLoop { body, .. } => out.push(*body),
            InstData::Match { scrutinee, arms } => {
                out.push(*scrutinee);
                for (pattern, body) in self.match_arms(arms).iter() {
                    out.push(body);
                    if let RirPatternView::Path {
                        module, ctor_head, ..
                    } = pattern
                    {
                        out.extend(module);
                        out.extend(ctor_head);
                    }
                }
            }
            InstData::Break { value } | InstData::Ret(value) => out.extend(value.iter().copied()),
            InstData::FnDecl { body, .. } | InstData::DropFnDecl { body, .. } => out.push(*body),
            InstData::ConstDecl { init, .. } | InstData::Alloc { init, .. } => out.push(*init),
            InstData::Call { args, .. } => {
                out.extend(self.call_args(args).values().map(|arg| arg.value))
            }
            InstData::MethodCall { receiver, args, .. } => {
                out.push(*receiver);
                out.extend(self.call_args(args).values().map(|arg| arg.value));
            }
            InstData::Intrinsic { args, .. } => out.extend(self.intrinsic_args(args).values()),
            InstData::InternalIntrinsic { args, .. } => {
                out.extend(self.internal_intrinsic_args(args).values())
            }
            InstData::Block { instructions } => out.extend(self.block_insts(instructions).values()),
            InstData::Assign { value, .. } => out.push(*value),
            InstData::PlaceSet { place, value } => out.extend([*place, *value]),
            InstData::StructDecl { methods, .. } => {
                out.extend(self.struct_methods(methods).values())
            }
            InstData::AnonStructType { methods, .. } => {
                out.extend(self.anon_struct_methods(methods).values())
            }
            InstData::StructInit {
                module,
                ctor_head,
                fields,
                ..
            } => {
                out.extend(module.iter().copied());
                out.extend(ctor_head.iter().copied());
                out.extend(self.field_inits(fields).values().map(|(_, value)| value));
            }
            InstData::FieldGet { base, .. } => out.push(*base),
            InstData::FieldSet { base, value, .. } => out.extend([*base, *value]),
            InstData::EnumVariant { module, .. } => out.extend(module.iter().copied()),
            InstData::ArrayInit { elements } => out.extend(self.array_elements(elements).values()),
            InstData::ArrayRepeat { value, .. } => out.push(*value),
            InstData::IndexGet {
                base,
                index: subscript,
            } => out.extend([*base, *subscript]),
            InstData::IndexSet {
                base,
                index: subscript,
                value,
            } => out.extend([*base, *subscript, *value]),
        }
    }

    fn validate_directive_range(&self, range: &RirDirectivesRange) -> Result<(), RirPayloadError> {
        self.validate_variable_records(
            range,
            |r| (r.start(), r.extent(), RirDirectivesRange::FAMILY),
            decoded_directive_record_extent,
        )?;
        let words = self.payload_words(range, |r| {
            (r.start(), r.extent(), RirDirectivesRange::FAMILY)
        })?;
        if words.is_empty() {
            return Ok(());
        }
        let mut position = 1usize;
        for record in 0..words[0] as usize {
            let record_width = decoded_directive_record_extent(words, position)
                .expect("variable-record validation established directive extent");
            if embedded_span(words, position).is_none() {
                return Err(rir_payload_error! {
                    family: RirDirectivesRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "directive span overflows u32",
                });
            }
            let arg_count = words[position + DIRECTIVE_ARG_COUNT] as usize;
            let args_start = position + DIRECTIVE_ARGS_START;
            let args_end = args_start + arg_count;
            if words[args_start..args_end]
                .iter()
                .any(|word| decode_symbol_word(*word).is_none())
            {
                return Err(rir_payload_error! {
                    family: RirDirectivesRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "symbol word is not representable",
                });
            }
            let (_, record_extent) =
                decode_directive_record(words, position, false).ok_or_else(|| {
                    rir_payload_error! {
                        family: RirDirectivesRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "directive record failed schema decoding",
                    }
                })?;
            let end = position + record_extent;
            position = end;
        }
        Ok(())
    }

    fn validate_enum_payload_words<R>(
        &self,
        range: &R,
        variants: usize,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.is_empty() {
            return Ok(());
        }
        let mut pos = 0usize;
        for record in 0..variants {
            if words.get(pos).is_none() {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: 1,
                    actual: 0,
                    reason: "missing variant payload record",
                });
            }
            let record_width = 1usize.saturating_add(words[pos] as usize);
            let Some((payload_start, end)) = enum_payload_record(words, pos) else {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: record_width,
                    actual: words.len().saturating_sub(pos).min(record_width),
                    reason: "variant payload record is truncated",
                });
            };
            // Payload words are declaration-local structured type references.
            // Their owner bounds are checked by `validate_context` after the
            // variable-width envelope has been proven complete here.
            let _ = payload_start;
            pos = end;
        }
        if pos != words.len() {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some(variants as u32),
                expected: 0,
                actual: words.len().saturating_sub(pos),
                reason: "trailing words after variant payloads",
            });
        }
        Ok(())
    }

    fn validate_enum_payload_range(
        &self,
        range: &RirEnumPayloadsRange,
        variants: usize,
    ) -> Result<(), RirPayloadError> {
        self.validate_enum_payload_words(range, variants, |r| {
            (r.start(), r.extent(), RirEnumPayloadsRange::FAMILY)
        })
    }

    fn validate_anon_enum_payload_range(
        &self,
        range: &RirAnonEnumPayloadsRange,
        variants: usize,
    ) -> Result<(), RirPayloadError> {
        self.validate_enum_payload_words(range, variants, |r| {
            (r.start(), r.extent(), RirAnonEnumPayloadsRange::FAMILY)
        })
    }
}
