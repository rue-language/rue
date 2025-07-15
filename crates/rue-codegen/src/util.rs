/// Utility functions for code generation
/// Aligns a value to a 16-byte boundary by rounding up
pub fn align_to_16(n: u64) -> u64 {
    (n + 15) & !15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_to_16() {
        assert_eq!(align_to_16(0), 0);
        assert_eq!(align_to_16(1), 16);
        assert_eq!(align_to_16(8), 16);
        assert_eq!(align_to_16(15), 16);
        assert_eq!(align_to_16(16), 16);
        assert_eq!(align_to_16(17), 32);
        assert_eq!(align_to_16(32), 32);
    }
}
