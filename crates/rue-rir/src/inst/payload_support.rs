use super::*;

impl RirPayloadError {
    pub(in crate::inst::payload) fn new(
        family: &'static str,
        start: u32,
        extent: u32,
        record: Option<u32>,
        expected_width: usize,
        actual_width: usize,
        reason: &'static str,
    ) -> Self {
        Self {
            family,
            start,
            extent,
            record,
            expected_width,
            actual_width,
            reason,
        }
    }
    pub const fn phase(&self) -> &'static str {
        "RIR payload decode"
    }
    pub fn expected_width(&self) -> usize {
        self.expected_width
    }
    pub fn actual_width(&self) -> usize {
        self.actual_width
    }
}

impl fmt::Display for RirPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: corrupt {} payload at start={} extent={}{}: expected width={}, actual width={}: {}",
            self.phase(),
            self.family,
            self.start,
            self.extent,
            self.record
                .map(|record| format!(", record {record}"))
                .unwrap_or_default(),
            self.expected_width(),
            self.actual_width(),
            self.reason
        )
    }
}
impl std::error::Error for RirPayloadError {}

impl Rir {
    /// Account for every owner-issued family on demand. Construction carries
    /// no counters or family-name searches in production.
    pub fn payload_storage_stats(&self) -> RirPayloadStorageStats {
        let mut stats = RirPayloadStorageStats {
            word_store_logical_bytes: self.extra.len() * std::mem::size_of::<u32>(),
            word_store_capacity_bytes: self.extra.capacity() * std::mem::size_of::<u32>(),
            ..RirPayloadStorageStats::default()
        };
        let mut account = |family: usize, extent: u32, envelope: bool| {
            let bytes = extent as usize * std::mem::size_of::<u32>();
            stats.family_logical_bytes[family] += bytes;
            // Only these five builders still materialize complete scratch
            // payloads before the atomic append. All other RIR families
            // reserve and encode directly into owner storage.
            if matches!(family, 1 | 12 | 13 | 14 | 15) {
                stats.peak_staging_bytes = stats.peak_staging_bytes.max(bytes);
            }
            stats.nonempty_variable_envelopes += usize::from(envelope && extent != 0);
        };
        for inst in &self.instructions {
            match &inst.data {
                InstData::Match { arms, .. } => account(0, arms.extent(), true),
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    account(1, directives.extent(), true);
                    account(2, params.extent(), false);
                }
                InstData::ConstDecl { directives, .. } | InstData::Alloc { directives, .. } => {
                    account(1, directives.extent(), true)
                }
                InstData::Call { args, .. } | InstData::MethodCall { args, .. } => {
                    account(3, args.extent(), false)
                }
                InstData::Intrinsic { args, .. } => account(4, args.extent(), false),
                InstData::InternalIntrinsic { args, .. } => account(5, args.extent(), false),
                InstData::Block { instructions } => account(6, instructions.extent(), false),
                InstData::StructDecl {
                    directives,
                    fields,
                    methods,
                    ..
                } => {
                    account(1, directives.extent(), true);
                    account(7, fields.extent(), false);
                    account(9, methods.extent(), false);
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    account(8, fields.extent(), false);
                    account(10, methods.extent(), false);
                }
                InstData::StructInit { fields, .. } => account(11, fields.extent(), false),
                InstData::EnumDecl {
                    variants, payloads, ..
                } => {
                    account(12, variants.extent(), false);
                    account(14, payloads.extent(), true);
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    account(13, variants.extent(), false);
                    account(15, payloads.extent(), true);
                }
                InstData::ArrayInit { elements } => account(16, elements.extent(), false),
                _ => {}
            }
        }
        stats
    }

    /// Safe fuzzing hook for the owner-local production decoders.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fuzz-support"))]
    pub fn fuzz_payload_corruption(input: &[u8]) -> Result<(), RirPayloadError> {
        #[derive(Clone, Copy)]
        struct ProbeRange {
            start: u32,
            extent: u32,
        }
        let family_index =
            input.first().copied().unwrap_or(0) as usize % RIR_PAYLOAD_FAMILY_NAMES.len();
        let operation = input.get(1).copied().unwrap_or(0) % 4;
        let family = RIR_PAYLOAD_FAMILY_NAMES[family_index];
        let mut rir = Rir::new();
        rir.extra = input
            .get(2..)
            .unwrap_or_default()
            .chunks(4)
            .take(32)
            .map(|chunk| {
                let mut word = [0; 4];
                word[..chunk.len()].copy_from_slice(chunk);
                u32::from_le_bytes(word)
            })
            .collect();
        if rir.extra.is_empty() {
            rir.extra.push(0);
        }
        let range = match operation {
            0 => ProbeRange {
                start: 0,
                extent: u32::try_from(rir.extra.len() + 1).unwrap(),
            },
            1 => ProbeRange {
                start: u32::MAX,
                extent: 2,
            },
            2 => ProbeRange {
                start: 1,
                extent: 0,
            },
            _ => ProbeRange {
                start: 0,
                extent: u32::try_from(rir.extra.len()).unwrap(),
            },
        };
        let parts = |r: &ProbeRange| (r.start, r.extent, family);
        match family_index {
            0 => rir.validate_variable_records(&range, parts, decoded_match_record_extent),
            1 => rir.validate_variable_records(&range, parts, decoded_directive_record_extent),
            2 => rir.validate_fixed(&range, PARAM_SCHEMA.width, parts),
            3 => rir.validate_fixed(&range, CALL_ARG_SCHEMA.width, parts),
            7 | 8 => rir.validate_fixed(&range, FIELD_DECL_SCHEMA.width, parts),
            11 => rir.validate_fixed(&range, FIELD_INIT_SCHEMA.width, parts),
            12 | 13 => rir.validate_fixed(&range, SYMBOL_SCHEMA.width, parts),
            14 | 15 => rir.validate_enum_payload_words(&range, 1, parts),
            _ => rir.validate_fixed(&range, REF_SCHEMA.width, parts),
        }
    }
}
