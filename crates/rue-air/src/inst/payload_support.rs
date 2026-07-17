use super::*;

impl Air {
    /// Safe owner-local fuzzing hook for checked payload decoding.
    #[doc(hidden)]
    #[cfg(any(test, feature = "fuzz-support"))]
    pub fn fuzz_payload_corruption(input: &[u8]) -> Result<(), AirPayloadError> {
        let family_index =
            input.first().copied().unwrap_or(0) as usize % AIR_PAYLOAD_FAMILY_NAMES.len();
        let operation = input.get(1).copied().unwrap_or(0) % 4;
        let family = AIR_PAYLOAD_FAMILY_NAMES[family_index];
        let mut air = Air::new(Type::UNIT);
        air.extra = input
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
        if air.extra.is_empty() {
            air.extra.push(0);
        }
        let (start, extent) = match operation {
            0 => (0, u32::try_from(air.extra.len() + 1).unwrap()),
            1 => (u32::MAX, 2),
            2 => (1, 0),
            _ => (0, u32::try_from(air.extra.len()).unwrap()),
        };
        let result = match family_index {
            0 => air
                .validate_match_arms(&AirMatchArms { start, extent })
                .map(|_| ()),
            1 => air
                .validate_call_args(&AirCallArgs { start, extent })
                .map(|_| ()),
            2 => air
                .try_get_types(&AirTypeArgs { start, extent })
                .map(|_| ()),
            3 => air
                .try_get_const_values(&AirConstValueWords { start, extent })
                .map(|_| ()),
            4 | 5 | 6 | 8 | 9 => air.try_get_refs(start, extent, family).map(|_| ()),
            7 => air.try_get_words(start, extent, family).map(|_| ()),
            _ => unreachable!(),
        };
        result.map_err(|mut error| {
            error.family = family;
            error
        })
    }
}
