//! SIGSEGV classification: stack overflow, or an ordinary segmentation fault
//! (RUE-2163).
//!
//! Before `checked` blocks and raw pointers (spec chapter 9) existed, a
//! `SIGSEGV` in a compiled Rue program could only be a blown stack, so RUE-645's
//! handler reported every one of them as `stack overflow`. That premise is gone:
//! `checked { @ptr_write(@int_to_ptr(0), 7) }` faults on a null pointer, and a
//! C FFI callee can fault anywhere at all. The handler now reads the faulting
//! address out of the `siginfo_t` the kernel supplies and decides between the
//! two, so a wild write is no longer reported as a stack overflow.
//!
//! # The decision rule
//!
//! A fault is a **stack overflow** exactly when its address lies in
//!
//! ```text
//!   [ stack_top - (stack_window + GUARD_SLACK) , stack_top ]
//! ```
//!
//! where
//!
//! - `stack_top` is the stack pointer the runtime captured at process entry,
//!   before any user code ran. On the Linux targets that is the untouched
//!   initial `rsp`/`sp` the kernel supplied (the same value `process.rs` reads
//!   argc/argv/envp from); on macOS it is `sp` on entry to `_main`. Either is
//!   within a few kilobytes below the true base of the main stack, and
//!   *under*estimating the base only widens the window downward, which cannot
//!   lose a real overflow.
//! - `stack_window` is `RLIMIT_STACK`'s soft limit when the platform reports a
//!   plausible finite one — greater than zero and at most [`MAX_STACK_WINDOW`]
//!   — and [`FALLBACK_STACK_WINDOW`] otherwise. The clamp is what keeps
//!   `ulimit -s unlimited` from turning the window into "every address below
//!   the stack", which would classify every wild pointer as an overflow again.
//! - [`GUARD_SLACK`] covers the kernel's guard region below the stack's lowest
//!   addressable byte (Linux's default `stack_guard_gap` is 1 MiB), because a
//!   frame larger than the remaining space can fault below the limit rather
//!   than at it.
//!
//! Everything else is reported as a segmentation fault at its address.
//!
//! # Why this cannot confuse the two shapes we care about
//!
//! Unbounded recursion walks the stack pointer down one frame at a time and
//! faults within one guard region of `stack_top - stack_window`, which is
//! inside the window by construction. A null or wild pointer write faults at a
//! small address — `0x0`, `0xdead0000` — while every supported target places
//! the main stack gigabytes above that, and the window reaches at most
//! `MAX_STACK_WINDOW + GUARD_SLACK` below its base. The two ranges cannot meet.
//!
//! A large *stride* — indexing an enormous offset off a stack object — is the
//! shape the rule genuinely cannot name, and it is reported as a segmentation
//! fault at the address it touched, which is the more useful of the two
//! answers.

use core::sync::atomic::{AtomicUsize, Ordering};

/// The `SA_SIGINFO` handler shape every platform installs: the signal number,
/// the kernel's `siginfo_t`, and the interrupted context.
///
/// The `siginfo_t` is passed as an opaque pointer because its layout is
/// per-platform; each platform module decodes it in its own `fault_address`.
pub(crate) type SegvHandler = extern "C" fn(i32, *const u8, *const u8) -> !;

/// Slack below the stack's lowest addressable byte that still counts as a
/// stack overflow. Linux's default `stack_guard_gap` is 256 pages (1 MiB); the
/// same allowance covers macOS, whose guard region is smaller.
pub(crate) const GUARD_SLACK: usize = 1 << 20;

/// The window used when the platform cannot report `RLIMIT_STACK`: the 8 MiB
/// soft limit both Linux and macOS default to.
pub(crate) const FALLBACK_STACK_WINDOW: usize = 8 << 20;

/// The largest `RLIMIT_STACK` taken at face value. A larger (or infinite) limit
/// falls back to [`FALLBACK_STACK_WINDOW`]: a window that reaches gigabytes
/// below the stack would swallow the wild-pointer addresses this
/// classification exists to distinguish.
pub(crate) const MAX_STACK_WINDOW: usize = 1 << 30;

/// The captured stack base. Zero until the entry code records the window.
static STACK_TOP: AtomicUsize = AtomicUsize::new(0);

/// The lowest address that still counts as a stack overflow.
static STACK_LOW: AtomicUsize = AtomicUsize::new(0);

/// Number of hex digits in a full address.
const ADDRESS_DIGITS: usize = (usize::BITS / 4) as usize;

/// Length of the `"segmentation fault at 0x"` prefix.
const SEGFAULT_PREFIX_LEN: usize = 24;

/// Buffer size the longest segmentation-fault message needs: the fixed prefix,
/// a full-width address, and the newline.
pub(crate) const SEGFAULT_MESSAGE_MAX: usize = SEGFAULT_PREFIX_LEN + ADDRESS_DIGITS + 1;

/// The stack window `stack_top` and `stack_limit` imply, as
/// `(highest, lowest)` addresses that count as a stack overflow.
///
/// Split out from [`record_stack_window`] so the rule itself is a pure
/// function with unit tests; the statics only carry its answer to the handler.
pub(crate) fn stack_window(stack_top: usize, stack_limit: Option<usize>) -> (usize, usize) {
    let window = match stack_limit {
        Some(limit) if limit > 0 && limit <= MAX_STACK_WINDOW => limit,
        _ => FALLBACK_STACK_WINDOW,
    };
    let low = stack_top.saturating_sub(window).saturating_sub(GUARD_SLACK);
    (stack_top, low)
}

/// Record the stack window the SIGSEGV handler classifies against.
///
/// Called from each platform's entry function before user code runs, so the
/// handler itself makes no syscalls and reads only these two words. Rue has no
/// threads, and the write happens-before any fault, so `Relaxed` suffices; the
/// atomics exist to avoid `static mut`.
pub(crate) fn record_stack_window(stack_top: usize, stack_limit: Option<usize>) {
    let (top, low) = stack_window(stack_top, stack_limit);
    STACK_TOP.store(top, Ordering::Relaxed);
    STACK_LOW.store(low, Ordering::Relaxed);
}

/// Whether a fault at `address` is a stack overflow under the rule above.
///
/// A window of zero means the entry code never recorded one, which no
/// supported entry path leaves possible; the defensive answer is RUE-645's
/// original one, since a runtime that cannot locate its own stack has no
/// grounds to contradict the handler's historical verdict.
pub(crate) fn is_stack_overflow(address: usize) -> bool {
    let top = STACK_TOP.load(Ordering::Relaxed);
    if top == 0 {
        return true;
    }
    address <= top && address >= STACK_LOW.load(Ordering::Relaxed)
}

/// Render `segmentation fault at 0x<address>\n` into `buffer`, returning the
/// written bytes.
///
/// The address is lowercase hex with no padding, so a null write reports
/// `0x0`. The prefix is assembled byte-by-byte rather than copied from a byte
/// string, for the macOS linker reason the other pinned runtime messages
/// document.
pub(crate) fn segfault_message(address: usize, buffer: &mut [u8; SEGFAULT_MESSAGE_MAX]) -> &[u8] {
    buffer[0] = b's';
    buffer[1] = b'e';
    buffer[2] = b'g';
    buffer[3] = b'm';
    buffer[4] = b'e';
    buffer[5] = b'n';
    buffer[6] = b't';
    buffer[7] = b'a';
    buffer[8] = b't';
    buffer[9] = b'i';
    buffer[10] = b'o';
    buffer[11] = b'n';
    buffer[12] = b' ';
    buffer[13] = b'f';
    buffer[14] = b'a';
    buffer[15] = b'u';
    buffer[16] = b'l';
    buffer[17] = b't';
    buffer[18] = b' ';
    buffer[19] = b'a';
    buffer[20] = b't';
    buffer[21] = b' ';
    buffer[22] = b'0';
    buffer[23] = b'x';

    let mut len = SEGFAULT_PREFIX_LEN;
    let mut shift = usize::BITS - 4;
    let mut started = false;
    loop {
        let digit = ((address >> shift) & 0xf) as u8;
        if digit != 0 || started || shift == 0 {
            started = true;
            buffer[len] = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + (digit - 10)
            };
            len += 1;
        }
        if shift == 0 {
            break;
        }
        shift -= 4;
    }

    buffer[len] = b'\n';
    len += 1;
    &buffer[..len]
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::string::String;

    /// A plausible main-stack base on the supported targets.
    const TOP: usize = 0x0000_7fff_ffff_e000;

    fn render(address: usize) -> String {
        let mut buffer = [0u8; SEGFAULT_MESSAGE_MAX];
        String::from_utf8(segfault_message(address, &mut buffer).to_vec()).expect("ASCII")
    }

    #[test]
    fn a_null_fault_renders_an_unpadded_zero() {
        assert_eq!(render(0), "segmentation fault at 0x0\n");
    }

    #[test]
    fn an_address_renders_in_lowercase_hex_without_padding() {
        assert_eq!(render(0xdead_0000), "segmentation fault at 0xdead0000\n");
        assert_eq!(render(0xabc), "segmentation fault at 0xabc\n");
    }

    #[test]
    fn a_full_width_address_fits_the_buffer() {
        assert_eq!(
            render(usize::MAX),
            "segmentation fault at 0xffffffffffffffff\n"
        );
    }

    #[test]
    fn the_window_is_the_reported_limit_plus_the_guard_slack() {
        let (top, low) = stack_window(TOP, Some(8 << 20));
        assert_eq!(top, TOP);
        assert_eq!(low, TOP - (8 << 20) - GUARD_SLACK);
    }

    /// An absent, zero, or implausibly large limit (`ulimit -s unlimited`
    /// reports `RLIM_INFINITY`) falls back to the 8 MiB default rather than
    /// widening the window until it swallows wild pointers.
    #[test]
    fn an_unusable_limit_falls_back_to_the_default_window() {
        let expected = TOP - FALLBACK_STACK_WINDOW - GUARD_SLACK;
        assert_eq!(stack_window(TOP, None).1, expected);
        assert_eq!(stack_window(TOP, Some(0)).1, expected);
        assert_eq!(stack_window(TOP, Some(usize::MAX)).1, expected);
        assert_eq!(stack_window(TOP, Some(MAX_STACK_WINDOW + 1)).1, expected);
    }

    /// A stack base near zero (no supported target has one) must not wrap.
    #[test]
    fn a_low_stack_base_saturates_instead_of_wrapping() {
        assert_eq!(stack_window(4096, Some(8 << 20)).1, 0);
    }

    /// The two shapes the classification exists to separate, decided against a
    /// recorded window. `record_stack_window` writes process-global statics, so
    /// one test drives every case rather than racing parallel ones.
    #[test]
    fn overflow_addresses_classify_apart_from_wild_ones() {
        record_stack_window(TOP, Some(8 << 20));

        // Unbounded recursion: the fault lands just inside the limit, and a
        // large frame can land inside the guard region just below it.
        assert!(is_stack_overflow(TOP - (8 << 20) + 0x1000));
        assert!(is_stack_overflow(TOP - (8 << 20) - 0x1000));
        assert!(is_stack_overflow(TOP));

        // A null or wild pointer write, and an address above the stack base.
        assert!(!is_stack_overflow(0));
        assert!(!is_stack_overflow(0xdead_0000));
        assert!(!is_stack_overflow(TOP + 0x1000));

        // A window that was never recorded keeps RUE-645's answer.
        STACK_TOP.store(0, Ordering::Relaxed);
        STACK_LOW.store(0, Ordering::Relaxed);
        assert!(is_stack_overflow(0));
    }
}
