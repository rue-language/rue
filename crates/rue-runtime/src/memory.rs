//! Memory intrinsics required by LLVM/rustc in no_std environments.
//!
//! These functions provide the same functionality as libc (memcpy, memmove, etc.)
//! but are implemented in pure Rust without external dependencies.

const CHUNK_SIZE: usize = core::mem::size_of::<u64>();

// These helpers deliberately use unaligned u64 accesses. The caller contracts
// below permit arbitrary byte alignment. Inline assembly keeps the individual
// accesses in the surrounding loops while making them opaque to LLVM's
// loop-idiom lowering; otherwise an optimized test binary can lower a byte
// loop back to an exported reserved primitive and recursively re-enter it.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn read_chunk(src: *const u8) -> u64 {
    let value;
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid input range. x86 permits an unaligned integer load.
    unsafe {
        core::arch::asm!(
            "mov {value}, [{src}]",
            value = out(reg) value,
            src = in(reg) src,
            options(nostack, preserves_flags, readonly),
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn write_chunk(dst: *mut u8, value: u64) {
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid output range. x86 permits an unaligned integer store.
    unsafe {
        core::arch::asm!(
            "mov [{dst}], {value}",
            dst = in(reg) dst,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn read_chunk(src: *const u8) -> u64 {
    let value;
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid input range. AArch64 permits an unaligned integer load.
    unsafe {
        core::arch::asm!(
            "ldr {value}, [{src}]",
            value = out(reg) value,
            src = in(reg) src,
            options(nostack, preserves_flags, readonly),
        );
    }
    value
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn write_chunk(dst: *mut u8, value: u64) {
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid output range. AArch64 permits an unaligned integer store.
    unsafe {
        core::arch::asm!(
            "str {value}, [{dst}]",
            dst = in(reg) dst,
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
unsafe fn read_chunk(src: *const u8) -> u64 {
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid input range. `read_unaligned` places no alignment requirement
    // on the pointer.
    unsafe { core::ptr::read_unaligned(src.cast::<u64>()) }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
unsafe fn write_chunk(dst: *mut u8, value: u64) {
    // SAFETY: Every caller has established that the complete chunk is inside
    // its valid output range. `write_unaligned` places no alignment
    // requirement on the pointer.
    unsafe { core::ptr::write_unaligned(dst.cast::<u64>(), value) }
}

#[inline(never)]
unsafe fn copy_forward_tail(dst: *mut u8, src: *const u8, remaining: usize) {
    match remaining {
        0 => {}
        1 => unsafe { *dst = *src },
        2 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
        },
        3 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
            *dst.add(2) = *src.add(2);
        },
        4 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
            *dst.add(2) = *src.add(2);
            *dst.add(3) = *src.add(3);
        },
        5 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
            *dst.add(2) = *src.add(2);
            *dst.add(3) = *src.add(3);
            *dst.add(4) = *src.add(4);
        },
        6 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
            *dst.add(2) = *src.add(2);
            *dst.add(3) = *src.add(3);
            *dst.add(4) = *src.add(4);
            *dst.add(5) = *src.add(5);
        },
        7 => unsafe {
            *dst = *src;
            *dst.add(1) = *src.add(1);
            *dst.add(2) = *src.add(2);
            *dst.add(3) = *src.add(3);
            *dst.add(4) = *src.add(4);
            *dst.add(5) = *src.add(5);
            *dst.add(6) = *src.add(6);
        },
        _ => unsafe { core::hint::unreachable_unchecked() },
    }
}

#[inline(never)]
unsafe fn copy_backward_tail(dst: *mut u8, src: *const u8, remaining: usize) {
    match remaining {
        0 => {}
        1 => unsafe { *dst = *src },
        2 => unsafe {
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        3 => unsafe {
            *dst.add(2) = *src.add(2);
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        4 => unsafe {
            *dst.add(3) = *src.add(3);
            *dst.add(2) = *src.add(2);
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        5 => unsafe {
            *dst.add(4) = *src.add(4);
            *dst.add(3) = *src.add(3);
            *dst.add(2) = *src.add(2);
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        6 => unsafe {
            *dst.add(5) = *src.add(5);
            *dst.add(4) = *src.add(4);
            *dst.add(3) = *src.add(3);
            *dst.add(2) = *src.add(2);
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        7 => unsafe {
            *dst.add(6) = *src.add(6);
            *dst.add(5) = *src.add(5);
            *dst.add(4) = *src.add(4);
            *dst.add(3) = *src.add(3);
            *dst.add(2) = *src.add(2);
            *dst.add(1) = *src.add(1);
            *dst = *src;
        },
        _ => unsafe { core::hint::unreachable_unchecked() },
    }
}

#[inline(never)]
unsafe fn set_tail(dst: *mut u8, value: u8, remaining: usize) {
    match remaining {
        0 => {}
        1 => unsafe { *dst = value },
        2 => unsafe {
            *dst = value;
            *dst.add(1) = value;
        },
        3 => unsafe {
            *dst = value;
            *dst.add(1) = value;
            *dst.add(2) = value;
        },
        4 => unsafe {
            *dst = value;
            *dst.add(1) = value;
            *dst.add(2) = value;
            *dst.add(3) = value;
        },
        5 => unsafe {
            *dst = value;
            *dst.add(1) = value;
            *dst.add(2) = value;
            *dst.add(3) = value;
            *dst.add(4) = value;
        },
        6 => unsafe {
            *dst = value;
            *dst.add(1) = value;
            *dst.add(2) = value;
            *dst.add(3) = value;
            *dst.add(4) = value;
            *dst.add(5) = value;
        },
        7 => unsafe {
            *dst = value;
            *dst.add(1) = value;
            *dst.add(2) = value;
            *dst.add(3) = value;
            *dst.add(4) = value;
            *dst.add(5) = value;
            *dst.add(6) = value;
        },
        _ => unsafe { core::hint::unreachable_unchecked() },
    }
}

#[inline(never)]
unsafe fn compare_bounded(a: *const u8, b: *const u8, remaining: usize) -> i32 {
    macro_rules! compare_byte {
        ($offset:expr) => {{
            // SAFETY: The caller established that the bounded range contains
            // every byte in this explicitly unrolled comparison.
            let left = unsafe { *a.add($offset) };
            let right = unsafe { *b.add($offset) };
            if left != right {
                return i32::from(left) - i32::from(right);
            }
        }};
    }
    match remaining {
        0 => {}
        1 => compare_byte!(0),
        2 => {
            compare_byte!(0);
            compare_byte!(1);
        }
        3 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
        }
        4 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
            compare_byte!(3);
        }
        5 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
            compare_byte!(3);
            compare_byte!(4);
        }
        6 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
            compare_byte!(3);
            compare_byte!(4);
            compare_byte!(5);
        }
        7 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
            compare_byte!(3);
            compare_byte!(4);
            compare_byte!(5);
            compare_byte!(6);
        }
        8 => {
            compare_byte!(0);
            compare_byte!(1);
            compare_byte!(2);
            compare_byte!(3);
            compare_byte!(4);
            compare_byte!(5);
            compare_byte!(6);
            compare_byte!(7);
        }
        _ => unsafe { core::hint::unreachable_unchecked() },
    }
    0
}

#[inline(always)]
unsafe fn copy_forward(mut dst: *mut u8, mut src: *const u8, mut remaining: usize) {
    while remaining >= CHUNK_SIZE {
        // SAFETY: The chunk is within both caller-provided ranges.
        let value = unsafe { read_chunk(src) };
        // SAFETY: The chunk is within the caller-provided destination range.
        unsafe { write_chunk(dst, value) };
        // SAFETY: The remaining range is valid, so advancing by one chunk
        // stays within the allocation (or one-past its end).
        dst = unsafe { dst.add(CHUNK_SIZE) };
        src = unsafe { src.add(CHUNK_SIZE) };
        remaining -= CHUNK_SIZE;
    }

    if remaining != 0 {
        // SAFETY: The remaining tail is smaller than one chunk.
        unsafe { copy_forward_tail(dst, src, remaining) };
    }
}

#[inline(always)]
unsafe fn copy_backward(dst: *mut u8, src: *const u8, mut remaining: usize) {
    while remaining >= CHUNK_SIZE {
        remaining -= CHUNK_SIZE;
        // SAFETY: The chunk is within both caller-provided ranges.
        let value = unsafe { read_chunk(src.add(remaining)) };
        // SAFETY: The chunk is within the caller-provided destination range.
        unsafe { write_chunk(dst.add(remaining), value) };
    }

    if remaining != 0 {
        // SAFETY: The remaining tail is smaller than one chunk.
        unsafe { copy_backward_tail(dst, src, remaining) };
    }
}

/// Copy `n` bytes from `src` to `dst`. The memory regions must not overlap.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - When `n > 0`, `src` must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
/// - The memory regions must not overlap
pub unsafe fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: The caller upholds the validity and non-overlap requirements;
    // copy_forward performs alignment-safe chunk and tail accesses.
    unsafe { copy_forward(dst, src, n) };
    dst
}

/// Copy `n` bytes from `src` to `dst`. The memory regions may overlap.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - When `n > 0`, `src` must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
pub unsafe fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize) < (src as usize) {
        // SAFETY: The caller upholds the validity requirements. Forward copy
        // is overlap-safe when the destination starts below the source.
        unsafe { copy_forward(dst, src, n) };
    } else {
        // SAFETY: The caller upholds the validity requirements. Backward copy
        // is overlap-safe when the destination starts at or above the source.
        unsafe { copy_backward(dst, src, n) };
    }
    dst
}

/// Fill `n` bytes of memory at `dst` with the byte `c`.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - `dst` may be null when `n == 0`
pub unsafe fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    let byte = c as u8;
    let chunk = u64::from_ne_bytes([byte; CHUNK_SIZE]);
    let mut remaining = n;
    let mut cursor = dst;
    while remaining >= CHUNK_SIZE {
        // SAFETY: The chunk is within the caller-provided destination range.
        unsafe { write_chunk(cursor, chunk) };
        cursor = unsafe { cursor.add(CHUNK_SIZE) };
        remaining -= CHUNK_SIZE;
    }
    if remaining != 0 {
        // SAFETY: The remaining tail is smaller than one chunk.
        unsafe { set_tail(cursor, byte, remaining) };
    }
    dst
}

/// `@byte_copy(dst, src, size)` runtime helper: copy `size` bytes from `src`
/// into the non-overlapping region at `dst` (ADR-0058, RUE-937).
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes and `src`
///   valid for reads of `size` bytes; the regions must not overlap.
/// - Either pointer may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_copy(dst: *mut u8, src: *const u8, size: u64) {
    // SAFETY: the caller upholds `memcpy`'s non-overlap and validity contract.
    unsafe { memcpy(dst, src, size as usize) };
}

/// `@byte_set(dst, value, size)` runtime helper: write the low byte of `value`
/// to each of `size` bytes at `dst` (ADR-0058, RUE-937).
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes.
/// - `dst` may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_set(dst: *mut u8, value: u64, size: u64) {
    // SAFETY: the caller upholds `memset`'s validity contract. `memset` masks
    // the fill value to its low byte, matching the intrinsic's `u8` operand.
    unsafe { memset(dst, value as i32, size as usize) };
}

/// `@byte_move(dst, src, size)` runtime helper: copy `size` bytes from `src`
/// into `dst` as if through a temporary buffer, so the two regions may overlap
/// (RUE-964). This is the memmove-shaped sibling of [`__rue_byte_copy`].
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes and `src`
///   valid for reads of `size` bytes. The regions may overlap.
/// - Either pointer may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_move(dst: *mut u8, src: *const u8, size: u64) {
    // SAFETY: the caller upholds `memmove`'s validity contract, which is
    // `memcpy`'s minus the non-overlap requirement.
    unsafe { memmove(dst, src, size as usize) };
}

/// Compare `n` bytes of memory at `s1` and `s2`.
///
/// Returns 0 if equal, negative if s1 < s2, positive if s1 > s2.
///
/// # Safety
///
/// - When `n > 0`, both pointers must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
pub unsafe fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut offset = 0;
    let mut remaining = n;
    while remaining >= CHUNK_SIZE {
        // SAFETY: The complete chunk is within both caller-provided ranges.
        let a = unsafe { read_chunk(s1.add(offset)) };
        let b = unsafe { read_chunk(s2.add(offset)) };
        if a != b {
            // Locate the first differing byte to preserve memcmp's ordering
            // and sign contract rather than comparing whole machine words.
            // SAFETY: The complete differing chunk is valid.
            return unsafe { compare_bounded(s1.add(offset), s2.add(offset), CHUNK_SIZE) };
        }
        offset += CHUNK_SIZE;
        remaining -= CHUNK_SIZE;
    }
    if remaining == 0 {
        return 0;
    }
    // SAFETY: The remaining tail is smaller than one chunk.
    unsafe { compare_bounded(s1.add(offset), s2.add(offset), remaining) }
}

/// Compare `n` bytes of memory at `s1` and `s2` for equality.
///
/// Returns 0 if equal, non-zero if different.
///
/// This is a simplified version of `memcmp` that only tests for equality,
/// not ordering. Some compilers (including rustc/LLVM) may generate calls
/// to `bcmp` for slice equality comparisons in no_std environments.
///
/// # Safety
///
/// - When `n > 0`, both pointers must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
#[inline(always)]
pub unsafe fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut offset = 0;
    let mut remaining = n;
    while remaining >= CHUNK_SIZE {
        // SAFETY: The complete chunk is within both caller-provided ranges.
        if unsafe { read_chunk(s1.add(offset)) != read_chunk(s2.add(offset)) } {
            return 1;
        }
        offset += CHUNK_SIZE;
        remaining -= CHUNK_SIZE;
    }
    if remaining == 0 {
        return 0;
    }
    // SAFETY: The remaining tail is smaller than one chunk.
    unsafe { (compare_bounded(s1.add(offset), s2.add(offset), remaining) != 0) as i32 }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::vec;
    use self::std::vec::Vec;
    use super::*;

    #[test]
    fn memcpy_handles_lengths_tails_and_all_alignments() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            for source_offset in 0..CHUNK_SIZE {
                for destination_offset in 0..CHUNK_SIZE {
                    let source = (0..128).map(|index| index as u8).collect::<Vec<_>>();
                    let mut destination = vec![0xa5; 128];
                    let destination_ptr =
                        unsafe { destination.as_mut_ptr().add(destination_offset) };
                    // SAFETY: Both slices contain the requested ranges and do
                    // not overlap.
                    let returned = unsafe {
                        memcpy(destination_ptr, source.as_ptr().add(source_offset), length)
                    };
                    assert_eq!(returned, destination_ptr);
                    assert!(
                        destination[..destination_offset]
                            .iter()
                            .all(|&byte| byte == 0xa5)
                    );
                    assert_eq!(
                        &destination[destination_offset..destination_offset + length],
                        &source[source_offset..source_offset + length]
                    );
                    assert!(
                        destination[destination_offset + length..]
                            .iter()
                            .all(|&byte| byte == 0xa5)
                    );
                }
            }
        }
    }

    #[test]
    fn memset_handles_lengths_tails_and_all_alignments() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            for destination_offset in 0..CHUNK_SIZE {
                let mut destination = vec![0xa5; 128];
                // SAFETY: The destination contains the requested range.
                let destination_ptr = unsafe { destination.as_mut_ptr().add(destination_offset) };
                let returned = unsafe { memset(destination_ptr, -0x12, length) };
                assert_eq!(returned, destination_ptr);
                assert!(
                    destination[..destination_offset]
                        .iter()
                        .all(|&byte| byte == 0xa5)
                );
                assert!(
                    destination[destination_offset..destination_offset + length]
                        .iter()
                        .all(|&byte| byte == 0xee)
                );
                assert!(
                    destination[destination_offset + length..]
                        .iter()
                        .all(|&byte| byte == 0xa5)
                );
            }
        }
    }

    #[test]
    fn memmove_handles_overlap_directions_distances_and_tails() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            for distance in 1..=(CHUNK_SIZE + 2) {
                for destination_after_source in [false, true] {
                    let source_start = CHUNK_SIZE + 8;
                    let destination_start = if destination_after_source {
                        source_start + distance
                    } else {
                        source_start - distance
                    };
                    let mut actual = (0..192).map(|index| index as u8).collect::<Vec<_>>();
                    let mut expected = actual.clone();
                    expected.copy_within(source_start..source_start + length, destination_start);
                    let destination_ptr = unsafe { actual.as_mut_ptr().add(destination_start) };
                    // SAFETY: Both ranges are within the backing allocation.
                    let returned = unsafe {
                        memmove(destination_ptr, actual.as_ptr().add(source_start), length)
                    };
                    assert_eq!(returned, destination_ptr);
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn memmove_handles_same_pointer_and_disjoint_allocations() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            let source_start = CHUNK_SIZE + 3;
            let mut same = (0..128).map(|index| index as u8).collect::<Vec<_>>();
            let expected = same.clone();
            let pointer = unsafe { same.as_mut_ptr().add(source_start) };
            // SAFETY: A zero-length or identical source/destination range is
            // valid under the memmove contract.
            let returned = unsafe { memmove(pointer, pointer, length) };
            assert_eq!(returned, pointer);
            assert_eq!(same, expected);

            let source = (0..128).map(|index| index as u8).collect::<Vec<_>>();
            let mut destination = vec![0xa5; 128];
            let destination_start = CHUNK_SIZE * 2 + 1;
            let destination_ptr = unsafe { destination.as_mut_ptr().add(destination_start) };
            // SAFETY: These are distinct allocations and both ranges are in bounds.
            let returned =
                unsafe { memmove(destination_ptr, source.as_ptr().add(source_start), length) };
            assert_eq!(returned, destination_ptr);
            assert_eq!(
                &destination[destination_start..destination_start + length],
                &source[source_start..source_start + length]
            );
            assert!(
                destination[..destination_start]
                    .iter()
                    .all(|&byte| byte == 0xa5)
            );
            assert!(
                destination[destination_start + length..]
                    .iter()
                    .all(|&byte| byte == 0xa5)
            );
        }
    }

    #[test]
    fn memcmp_preserves_first_mismatch_order_and_sign() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            for left_offset in 0..CHUNK_SIZE {
                for right_offset in 0..CHUNK_SIZE {
                    let mut left = vec![0x55; length + CHUNK_SIZE * 2];
                    let mut right = left.clone();
                    // SAFETY: Both pointers name valid ranges of `length` bytes.
                    assert_eq!(
                        unsafe {
                            memcmp(
                                left.as_ptr().add(left_offset),
                                right.as_ptr().add(right_offset),
                                length,
                            )
                        },
                        0
                    );
                    // SAFETY: The same pointer names a valid range, including
                    // the zero-length case.
                    assert_eq!(
                        unsafe {
                            memcmp(
                                left.as_ptr().add(left_offset),
                                left.as_ptr().add(left_offset),
                                length,
                            )
                        },
                        0
                    );
                    for mismatch in 0..length {
                        left[left_offset + mismatch] = 0x00;
                        right[right_offset + mismatch] = 0xff;
                        // SAFETY: Both pointers name valid ranges of `length` bytes.
                        let result = unsafe {
                            memcmp(
                                left.as_ptr().add(left_offset),
                                right.as_ptr().add(right_offset),
                                length,
                            )
                        };
                        assert!(result < 0, "length={length}, mismatch={mismatch}");

                        left[left_offset + mismatch] = 0xff;
                        right[right_offset + mismatch] = 0x00;
                        // SAFETY: Both pointers name valid ranges of `length` bytes.
                        let result = unsafe {
                            memcmp(
                                left.as_ptr().add(left_offset),
                                right.as_ptr().add(right_offset),
                                length,
                            )
                        };
                        assert!(result > 0, "length={length}, mismatch={mismatch}");
                        left[left_offset + mismatch] = 0x55;
                        right[right_offset + mismatch] = 0x55;
                    }
                }
            }
        }
    }

    #[test]
    fn bcmp_handles_chunks_tails_and_all_alignments() {
        for length in 0..=(CHUNK_SIZE * 4 + 3) {
            for left_offset in 0..CHUNK_SIZE {
                for right_offset in 0..CHUNK_SIZE {
                    let mut left = vec![0xa5; 128];
                    let mut right = vec![0x5a; 128];
                    for index in 0..=(CHUNK_SIZE * 4 + 3) {
                        let value = (index as u8).wrapping_mul(29).wrapping_add(7);
                        left[left_offset + index] = value;
                        right[right_offset + index] = value;
                    }
                    // SAFETY: Both pointers name valid ranges of `length` bytes.
                    assert_eq!(
                        unsafe {
                            bcmp(
                                left.as_ptr().add(left_offset),
                                right.as_ptr().add(right_offset),
                                length,
                            )
                        },
                        0
                    );
                    // SAFETY: The same pointer names a valid range.
                    assert_eq!(
                        unsafe {
                            bcmp(
                                left.as_ptr().add(left_offset),
                                left.as_ptr().add(left_offset),
                                length,
                            )
                        },
                        0
                    );
                    for mismatch in 0..length {
                        left[left_offset + mismatch] ^= 1;
                        // SAFETY: The changed byte remains within the compared range.
                        assert_ne!(
                            unsafe {
                                bcmp(
                                    left.as_ptr().add(left_offset),
                                    right.as_ptr().add(right_offset),
                                    length,
                                )
                            },
                            0
                        );
                        left[left_offset + mismatch] ^= 1;
                    }
                }
            }
        }
    }

    #[test]
    fn test_bcmp_equal() {
        let a = b"hello world";
        let b = b"hello world";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_not_equal() {
        let a = b"hello world";
        let b = b"hello xorld";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_empty() {
        let a = b"";
        let b = b"";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 0) };
        assert_eq!(result, 0);
    }

    #[test]
    fn zero_length_accepts_null_and_distinct_pointers() {
        let byte = 0u8;
        let pointer = &byte as *const u8;
        // SAFETY: All operations have zero length, so null pointers are valid
        // under their documented contracts.
        unsafe {
            assert!(memcpy(core::ptr::null_mut(), pointer, 0).is_null());
            assert!(memmove(pointer as *mut u8, core::ptr::null(), 0) == pointer as *mut u8);
            assert!(memset(core::ptr::null_mut(), 0, 0).is_null());
            assert_eq!(memcmp(core::ptr::null(), pointer, 0), 0);
            assert_eq!(bcmp(pointer, core::ptr::null(), 0), 0);
        }
    }

    #[test]
    fn rue_memory_wrappers_preserve_primitive_behavior() {
        let source = (0..64).map(|index| index as u8).collect::<Vec<_>>();
        let mut destination = vec![0xa5; 72];
        // SAFETY: Every wrapper range is valid, and the copy ranges do not overlap.
        unsafe {
            __rue_byte_copy(destination.as_mut_ptr().add(3), source.as_ptr().add(5), 35);
        }
        assert_eq!(&destination[3..38], &source[5..40]);
        let mut expected = destination.clone();
        expected.copy_within(3..38, 8);
        // SAFETY: Both ranges are within this allocation and may overlap.
        unsafe {
            __rue_byte_move(
                destination.as_mut_ptr().add(8),
                destination.as_ptr().add(3),
                35,
            )
        };
        assert_eq!(destination, expected);
        // SAFETY: The wrapper writes only within the destination range.
        unsafe { __rue_byte_set(destination.as_mut_ptr().add(11), 0x1ee, 17) };
        assert!(destination[11..28].iter().all(|&byte| byte == 0xee));
    }

    #[test]
    fn test_bcmp_first_byte_differs() {
        let a = b"abc";
        let b = b"xbc";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_last_byte_differs() {
        let a = b"abc";
        let b = b"abx";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_partial_comparison() {
        // Compare only first 3 bytes - they're the same
        let a = b"abcdef";
        let b = b"abcxyz";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 3) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_equal() {
        let a = [42u8];
        let b = [42u8];
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_differs() {
        let a = [42u8];
        let b = [43u8];
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_ne!(result, 0);
    }
}
