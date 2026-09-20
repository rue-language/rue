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
//! Freestanding builds keep one captured main-stack window in atomics because
//! they have no worker threads. Hosted builds instead publish the main stack
//! and each worker's bounds in fixed atomic slots keyed by the raw OS thread
//! identity. Linux worker bounds include the guard size reported by the
//! default pthread attributes; Darwin uses its default `vm_page_size` guard.
//! A future worker-attribute API must carry any non-default guard size into
//! registration rather than changing this classification implicitly.
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

#[cfg(all(test, rue_hosted_threads))]
extern crate std;

#[cfg(rue_hosted_threads)]
use core::marker::PhantomData;
#[cfg(rue_hosted_threads)]
use core::sync::atomic::{AtomicU8, AtomicU64};
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(test, rue_hosted_threads))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestFailurePoint {
    Reserve,
    Allocate,
    MutexInit,
    CondvarInit,
    Create,
    WorkerRegistration,
    WorkerInitialization,
}

#[cfg(all(test, rue_hosted_threads))]
impl TestFailurePoint {
    const fn code(self) -> u8 {
        match self {
            Self::Reserve => 1,
            Self::Allocate => 2,
            Self::MutexInit => 3,
            Self::CondvarInit => 4,
            Self::Create => 5,
            Self::WorkerRegistration => 6,
            Self::WorkerInitialization => 7,
        }
    }
}

#[cfg(all(test, rue_hosted_threads))]
static TEST_FAILURE: AtomicU8 = AtomicU8::new(0);

#[cfg(all(test, rue_hosted_threads))]
pub(crate) fn set_test_failure(point: TestFailurePoint) {
    assert_eq!(
        TEST_FAILURE.swap(point.code(), Ordering::SeqCst),
        0,
        "runtime failure injection is not nestable"
    );
}

#[cfg(all(test, rue_hosted_threads))]
pub(crate) fn clear_test_failure() {
    assert_eq!(
        TEST_FAILURE.swap(0, Ordering::SeqCst),
        0,
        "failure was not consumed"
    );
}

#[cfg(all(test, rue_hosted_threads))]
pub(crate) fn take_test_failure(point: TestFailurePoint) -> bool {
    TEST_FAILURE
        .compare_exchange(point.code(), 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[cfg(all(test, rue_hosted_threads))]
pub(crate) static REGISTRY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, rue_hosted_threads))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TestResourceCounts {
    pub(crate) slot_acquires: usize,
    pub(crate) slot_releases: usize,
    pub(crate) stack_allocations: usize,
    pub(crate) stack_destroys: usize,
    pub(crate) stack_registrations: usize,
    pub(crate) mutex_inits: usize,
    pub(crate) mutex_destroys: usize,
    pub(crate) condvar_inits: usize,
    pub(crate) condvar_destroys: usize,
    pub(crate) pthread_creates: usize,
    pub(crate) pthread_joins: usize,
}

#[cfg(all(test, rue_hosted_threads))]
static TEST_SLOT_ACQUIRES: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, rue_hosted_threads))]
static TEST_SLOT_RELEASES: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, rue_hosted_threads))]
static TEST_STACK_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, rue_hosted_threads))]
static TEST_STACK_DESTROYS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, rue_hosted_threads))]
static TEST_STACK_REGISTRATIONS: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, rue_hosted_threads))]
pub(crate) fn test_resource_counts() -> TestResourceCounts {
    TestResourceCounts {
        slot_acquires: TEST_SLOT_ACQUIRES.load(Ordering::Acquire),
        slot_releases: TEST_SLOT_RELEASES.load(Ordering::Acquire),
        stack_allocations: TEST_STACK_ALLOCATIONS.load(Ordering::Acquire),
        stack_destroys: TEST_STACK_DESTROYS.load(Ordering::Acquire),
        stack_registrations: TEST_STACK_REGISTRATIONS.load(Ordering::Acquire),
        mutex_inits: crate::join::test_mutex_inits(),
        mutex_destroys: crate::join::test_mutex_destroys(),
        condvar_inits: crate::join::test_condvar_inits(),
        condvar_destroys: crate::join::test_condvar_destroys(),
        pthread_creates: crate::join::test_pthread_creates(),
        pthread_joins: crate::join::test_pthread_joins(),
    }
}

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

#[cfg(not(rue_hosted_threads))]
/// The captured stack base. Zero until the entry code records the window.
static STACK_TOP: AtomicUsize = AtomicUsize::new(0);

#[cfg(not(rue_hosted_threads))]
/// The lowest address that still counts as a stack overflow.
static STACK_LOW: AtomicUsize = AtomicUsize::new(0);

#[cfg(rue_hosted_threads)]
const MAIN_SLOT: usize = 0;
#[cfg(rue_hosted_threads)]
pub(crate) const WORKER_SLOT_COUNT: usize = 64;
#[cfg(rue_hosted_threads)]
const SLOT_FREE: u8 = 0;
#[cfg(rue_hosted_threads)]
const SLOT_RESERVED: u8 = 1;
#[cfg(rue_hosted_threads)]
const SLOT_PUBLISHED: u8 = 2;

#[cfg(rue_hosted_threads)]
struct FaultSlot {
    state: AtomicU8,
    owner: AtomicU64,
    low: AtomicUsize,
    high: AtomicUsize,
}

#[cfg(rue_hosted_threads)]
impl FaultSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(SLOT_FREE),
            owner: AtomicU64::new(0),
            low: AtomicUsize::new(0),
            high: AtomicUsize::new(0),
        }
    }
}

#[cfg(rue_hosted_threads)]
static FAULT_SLOTS: [FaultSlot; WORKER_SLOT_COUNT] =
    [const { FaultSlot::new() }; WORKER_SLOT_COUNT];

/// A per-operation alternate signal stack. Its owner remains the parent
/// operation until join, even though the child registers it with sigaltstack.
#[cfg(rue_hosted_threads)]
pub(crate) struct AltSignalStack {
    pointer: *mut u8,
    size: usize,
}

#[cfg(rue_hosted_threads)]
impl AltSignalStack {
    /// Construct an owned mapping handle returned by a platform allocator.
    ///
    /// # Safety
    /// `pointer..pointer + size` must be one live mapping owned exclusively by
    /// this handle and suitable for `sigaltstack`; it must be unmapped exactly
    /// once by [`destroy_worker_alt_stack`] after the worker has joined.
    pub(crate) unsafe fn from_raw(pointer: *mut u8, size: usize) -> Self {
        Self { pointer, size }
    }

    pub(crate) fn raw_parts(&self) -> (*mut u8, usize) {
        (self.pointer, self.size)
    }
}

#[cfg(rue_hosted_threads)]
unsafe impl Send for AltSignalStack {}

#[cfg(rue_hosted_threads)]
pub(crate) struct WorkerSlotReservation {
    index: usize,
}

#[cfg(rue_hosted_threads)]
pub(crate) struct WorkerSlotToken {
    index: usize,
}

#[cfg(rue_hosted_threads)]
pub(crate) struct WorkerRegistration {
    index: usize,
    // A registration belongs to the worker that published it. Keeping this
    // marker makes accidental movement to a parent/foreign thread a compile
    // error; Drop must run on the owning pthread after user code finishes.
    _worker_only: PhantomData<*mut ()>,
}

#[cfg(rue_hosted_threads)]
impl WorkerSlotReservation {
    pub(crate) fn handoff(self) -> WorkerSlotToken {
        let token = WorkerSlotToken { index: self.index };
        core::mem::forget(self);
        token
    }
}

#[cfg(rue_hosted_threads)]
unsafe impl Send for WorkerSlotReservation {}

#[cfg(rue_hosted_threads)]
unsafe impl Send for WorkerSlotToken {}

#[cfg(rue_hosted_threads)]
impl Drop for WorkerSlotReservation {
    fn drop(&mut self) {
        #[cfg(test)]
        TEST_SLOT_RELEASES.fetch_add(1, Ordering::AcqRel);
        FAULT_SLOTS[self.index]
            .state
            .store(SLOT_FREE, Ordering::Release);
    }
}

#[cfg(rue_hosted_threads)]
impl Drop for WorkerSlotToken {
    fn drop(&mut self) {
        #[cfg(test)]
        TEST_SLOT_RELEASES.fetch_add(1, Ordering::AcqRel);
        FAULT_SLOTS[self.index]
            .state
            .store(SLOT_FREE, Ordering::Release);
    }
}

#[cfg(rue_hosted_threads)]
impl Drop for WorkerRegistration {
    fn drop(&mut self) {
        let slot = &FAULT_SLOTS[self.index];
        // The registration is dropped by its owning pthread after worker user
        // code finishes and before pthread exit. A foreign caller is a runtime
        // invariant violation: fail stop rather than leaving stale bounds
        // published.
        let owner = crate::platform::thread_id();
        assert_eq!(slot.owner.load(Ordering::Acquire), owner);
        assert_eq!(
            slot.state.load(Ordering::Acquire),
            SLOT_PUBLISHED,
            "worker registration dropped after slot state changed"
        );
        assert!(
            slot.state
                .compare_exchange(
                    SLOT_PUBLISHED,
                    SLOT_FREE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        );
        #[cfg(test)]
        TEST_SLOT_RELEASES.fetch_add(1, Ordering::AcqRel);
    }
}

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

#[cfg(rue_hosted_threads)]
pub(crate) fn reserve_worker_slot() -> Result<WorkerSlotReservation, i32> {
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::Reserve) {
        return Err(libc::EAGAIN);
    }
    for index in (MAIN_SLOT + 1)..WORKER_SLOT_COUNT {
        let slot = &FAULT_SLOTS[index];
        if slot
            .state
            .compare_exchange(
                SLOT_FREE,
                SLOT_RESERVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            #[cfg(test)]
            TEST_SLOT_ACQUIRES.fetch_add(1, Ordering::AcqRel);
            return Ok(WorkerSlotReservation { index });
        }
    }
    Err(libc::EAGAIN)
}

#[cfg(rue_hosted_threads)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn publish_main_stack(stack_top: usize, stack_limit: Option<usize>) -> Result<(), i32> {
    let slot = &FAULT_SLOTS[MAIN_SLOT];
    if slot
        .state
        .compare_exchange(
            SLOT_FREE,
            SLOT_RESERVED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return Err(libc::EBUSY);
    }
    let (high, low) = stack_window(stack_top, stack_limit);
    let owner = crate::platform::thread_id();
    if owner == 0 {
        slot.state.store(SLOT_FREE, Ordering::Release);
        return Err(libc::ESRCH);
    }
    slot.low.store(low, Ordering::Relaxed);
    slot.high.store(high, Ordering::Relaxed);
    slot.owner.store(owner, Ordering::Relaxed);
    slot.state.store(SLOT_PUBLISHED, Ordering::Release);
    Ok(())
}

#[cfg(rue_hosted_threads)]
pub(crate) fn initialize_main(stack_top: usize, stack_limit: Option<usize>) -> Result<(), i32> {
    crate::platform::install_main_alt_stack()?;
    publish_main_stack(stack_top, stack_limit)?;
    crate::platform::install_segv_disposition(crate::entry::__rue_segv_handler)
}

#[cfg(rue_hosted_threads)]
pub(crate) fn allocate_worker_alt_stack() -> Result<AltSignalStack, i32> {
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::Allocate) {
        return Err(libc::ENOMEM);
    }
    let result = crate::platform::allocate_thread_alt_stack();
    #[cfg(test)]
    if result.is_ok() {
        TEST_STACK_ALLOCATIONS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

#[cfg(rue_hosted_threads)]
/// Register one worker's alternate stack and publish its bounds.
///
/// # Safety
///
/// The caller must dedicate `stack` to this one pthread, keep its mapping
/// alive until that pthread has returned and been joined, and ensure no other
/// thread installs or uses the same mapping concurrently. The returned
/// registration must stay live while worker user code can execute, then be
/// dropped on this same pthread after user code finishes and before pthread
/// exit. The mapping remains live through the join, including any teardown
/// after the registration is dropped.
pub(crate) unsafe fn initialize_worker(
    token: WorkerSlotToken,
    stack: &AltSignalStack,
) -> Result<WorkerRegistration, i32> {
    // SAFETY: the function's contract dedicates this mapping to this one
    // pthread and keeps it alive until join.
    unsafe { crate::platform::register_thread_alt_stack(stack) }?;
    #[cfg(test)]
    TEST_STACK_REGISTRATIONS.fetch_add(1, Ordering::AcqRel);
    publish_worker_slot(token, crate::platform::worker_stack_bounds())
}

#[cfg(rue_hosted_threads)]
fn publish_worker_slot(
    token: WorkerSlotToken,
    bounds: Result<(usize, usize), i32>,
) -> Result<WorkerRegistration, i32> {
    let index = token.index;
    let slot = &FAULT_SLOTS[index];
    let owner = crate::platform::thread_id();
    if owner == 0 {
        return Err(libc::ESRCH);
    }
    let (low, high) = bounds?;
    slot.low.store(low, Ordering::Relaxed);
    slot.high.store(high, Ordering::Relaxed);
    slot.owner.store(owner, Ordering::Relaxed);
    slot.state.store(SLOT_PUBLISHED, Ordering::Release);
    core::mem::forget(token);
    Ok(WorkerRegistration {
        index,
        _worker_only: PhantomData,
    })
}

/// Test seam that preserves the real alt-stack registration, then injects a
/// bounds-query failure to exercise reservation rollback. The caller must
/// destroy the returned mapping after the worker thread has joined.
#[cfg(all(test, rue_hosted_threads))]
pub(crate) unsafe fn initialize_worker_with_bounds_for_test(
    token: WorkerSlotToken,
    stack: &AltSignalStack,
    bounds: Result<(usize, usize), i32>,
) -> Result<WorkerRegistration, i32> {
    // SAFETY: the test dedicates this mapping to the child and joins before
    // destroying it.
    unsafe { crate::platform::register_thread_alt_stack(stack) }?;
    TEST_STACK_REGISTRATIONS.fetch_add(1, Ordering::AcqRel);
    publish_worker_slot(token, bounds)
}

#[cfg(rue_hosted_threads)]
pub(crate) unsafe fn destroy_worker_alt_stack(stack: AltSignalStack) -> Result<(), i32> {
    // SAFETY: the caller proves that the worker has joined and no signal can
    // still execute on this mapping.
    let result = unsafe { crate::platform::destroy_thread_alt_stack(stack) };
    #[cfg(test)]
    if result.is_ok() {
        TEST_STACK_DESTROYS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

/// Record the stack window the SIGSEGV handler classifies against.
///
/// Called from each platform's entry function before user code runs, so the
/// handler itself makes no syscalls and reads only these two words. Rue has no
/// threads, and the write happens-before any fault, so `Relaxed` suffices; the
/// atomics exist to avoid `static mut`.
#[cfg(not(rue_hosted_threads))]
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
#[cfg(not(rue_hosted_threads))]
pub(crate) fn is_stack_overflow(address: usize) -> bool {
    let top = STACK_TOP.load(Ordering::Relaxed);
    if top == 0 {
        return true;
    }
    address <= top && address >= STACK_LOW.load(Ordering::Relaxed)
}

#[cfg(rue_hosted_threads)]
pub(crate) fn is_stack_overflow(address: usize) -> bool {
    let owner = crate::platform::thread_id();
    if owner == 0 {
        return false;
    }
    for slot in &FAULT_SLOTS {
        if slot.state.load(Ordering::Acquire) != SLOT_PUBLISHED {
            continue;
        }
        if slot.owner.load(Ordering::Acquire) != owner {
            continue;
        }
        let high = slot.high.load(Ordering::Relaxed);
        let low = slot.low.load(Ordering::Relaxed);
        return address <= high && address >= low;
    }
    false
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
    #[cfg(not(rue_hosted_threads))]
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

#[cfg(all(test, rue_hosted_threads))]
mod hosted_tests {
    extern crate std;

    use super::*;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};
    use std::vec::Vec;

    /// The registry is intentionally exercised in one test because its static
    /// slots are process-wide: this covers the exhaustion and reuse protocol
    /// without allowing parallel tests to hide a leaked reservation.
    #[test]
    fn worker_reservations_exhaust_reuse_and_publish_bounds() {
        let _serial = REGISTRY_TEST_LOCK.lock().unwrap();
        let mut reservations = Vec::new();
        for _ in 0..(WORKER_SLOT_COUNT - 1) {
            reservations.push(reserve_worker_slot().expect("reserve worker slot"));
        }
        assert!(matches!(reserve_worker_slot(), Err(error) if error == libc::EAGAIN));

        // Dropping a reservation rolls it back to FREE, so another launch can
        // claim the same capacity before any worker is started.
        drop(reservations.pop());
        let reservation = reserve_worker_slot().expect("reuse rolled-back slot");
        drop(reservation);
        drop(reservations);

        // A live registration publishes the current worker's exact stack
        // bounds. Its Drop path clears only the slot owned by this raw TID;
        // afterward an unregistered thread is conservatively a segfault.
        let reservation = reserve_worker_slot().expect("reserve registration slot");
        let token = reservation.handoff();
        let stack = allocate_worker_alt_stack().expect("allocate alternate stack");
        let handle = std::thread::spawn(move || {
            // SAFETY: this child owns the dedicated mapping through its return;
            // the parent joins before destroying it.
            let registration =
                unsafe { initialize_worker(token, &stack) }.expect("publish worker bounds");
            let (low, high) = crate::platform::worker_stack_bounds().expect("read stack bounds");
            assert!(is_stack_overflow(low));
            assert!(is_stack_overflow(high));
            assert!(!is_stack_overflow(0));
            drop(registration);
            (stack, low)
        });
        let (stack, low) = handle.join().expect("join worker registration");
        assert!(!is_stack_overflow(low));

        // The parent destroys the mapping only after the child joined, so no
        // pthread can still have this mapping installed as its alt stack.
        unsafe { destroy_worker_alt_stack(stack).expect("destroy alternate stack") };
    }

    #[test]
    fn bounds_failure_after_alt_stack_registration_rolls_back_reservation() {
        let _serial = REGISTRY_TEST_LOCK.lock().unwrap();
        let reservation = reserve_worker_slot().expect("reserve worker slot");
        let slot_index = reservation.index;
        let token = reservation.handoff();
        let stack = allocate_worker_alt_stack().expect("allocate worker alternate stack");
        let handle = thread::spawn(move || {
            // SAFETY: this child owns the dedicated mapping through its return;
            // the parent joins before destroying it.
            let result =
                unsafe { initialize_worker_with_bounds_for_test(token, &stack, Err(libc::ERANGE)) };
            assert!(matches!(result, Err(error) if error == libc::ERANGE));
            // The alt stack remains installed until this thread returns. The
            // parent owns and destroys the mapping only after join.
            stack
        });
        let stack = handle.join().expect("join failed worker initialization");
        let replacement = reserve_worker_slot().expect("failed initialization leaked a slot");
        assert_eq!(replacement.index, slot_index);
        drop(replacement);
        // This is intentionally after join: sigaltstack still references the
        // mapping for the failed worker until the pthread exits.
        unsafe { destroy_worker_alt_stack(stack).expect("destroy failed worker alt stack") };
    }

    #[test]
    fn a_slot_can_be_reused_before_the_parent_joins_the_worker() {
        let _serial = REGISTRY_TEST_LOCK.lock().unwrap();
        let reservation = reserve_worker_slot().expect("reserve worker slot");
        let slot_index = reservation.index;
        let token = reservation.handoff();
        let stack = allocate_worker_alt_stack().expect("allocate worker alternate stack");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (released_tx, released_rx) = std::sync::mpsc::channel();
        let (reuse_tx, reuse_rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            // SAFETY: the mapping is dedicated to this worker and remains live
            // until the parent joins before destroying it.
            let registration =
                unsafe { initialize_worker(token, &stack) }.expect("publish worker bounds");
            ready_tx.send(registration.index).unwrap();
            release_rx.recv().unwrap();
            drop(registration);
            released_tx.send(()).unwrap();
            reuse_rx.recv().unwrap();
            stack
        });
        assert_eq!(
            ready_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("worker did not publish its slot"),
            slot_index
        );

        // Ask the child to unpublish while it is still alive, then prove the
        // exact same slot is reusable while its mapping remains parent-owned.
        release_tx.send(()).unwrap();
        released_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker did not unpublish its slot");
        let replacement = reserve_worker_slot().expect("slot was not reusable");
        assert_eq!(replacement.index, slot_index);
        drop(replacement);
        reuse_tx.send(()).unwrap();
        let stack = handle.join().expect("join worker");
        unsafe { destroy_worker_alt_stack(stack).expect("destroy worker alt stack") };
    }

    #[test]
    fn registered_worker_faults_use_the_canonical_handler() {
        let _serial = REGISTRY_TEST_LOCK.lock().unwrap();
        const CHILD_ENV: &str = "RUE_REGISTERED_WORKER_FAULT";
        const CHILD_TEST: &str =
            "fault::hosted_tests::registered_worker_faults_use_the_canonical_handler";

        if let Ok(mode) = std::env::var(CHILD_ENV) {
            run_registered_worker_fault_child(&mode);
        }

        for (mode, expected) in [
            ("overflow", b"stack overflow\n".as_slice()),
            ("null", b"segmentation fault at 0x0\n".as_slice()),
        ] {
            let mut child = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", CHILD_TEST, "--nocapture"])
                .env(CHILD_ENV, mode)
                .env("__RUST_TEST_INVOKE", CHILD_TEST)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn registered-worker fault child");
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if child.try_wait().expect("poll fault child").is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("registered-worker {mode} fault child exceeded deadline");
                }
                thread::sleep(Duration::from_millis(10));
            }
            let output = child
                .wait_with_output()
                .expect("collect registered-worker fault child");
            assert_eq!(
                output.status.code(),
                Some(101),
                "child stderr: {:?}",
                output.stderr
            );
            assert_eq!(output.stderr, expected);
        }
    }

    fn run_registered_worker_fault_child(mode: &str) -> ! {
        let marker = 0usize;
        initialize_main(
            (&marker as *const usize) as usize,
            crate::platform::stack_limit(),
        )
        .expect("initialize hosted fault handler");
        let reservation = reserve_worker_slot().expect("reserve worker slot");
        let token = reservation.handoff();
        let stack = allocate_worker_alt_stack().expect("allocate worker alternate stack");
        let mode = match mode {
            "overflow" => 0u8,
            "null" => 1u8,
            _ => panic!("unknown registered-worker fault mode: {mode}"),
        };
        thread::spawn(move || {
            // SAFETY: the mapping is dedicated to this child and remains live
            // for its whole pthread lifetime (the process exits on a fault).
            let _registration =
                unsafe { initialize_worker(token, &stack) }.expect("register worker");
            if mode == 0 {
                recurse_until_stack_fault(0);
            } else {
                fault_null_pointer();
            }
        });
        loop {
            thread::park();
        }
    }

    #[inline(never)]
    #[allow(unconditional_recursion)]
    fn recurse_until_stack_fault(depth: usize) -> ! {
        // Keep a live, observable frame so this cannot become a tail-recursive
        // counter. The pthread guard must be crossed by an actual worker stack
        // exhaustion before the canonical handler runs on the alt stack.
        let mut frame = [0u8; 8192];
        unsafe { core::ptr::write_volatile(frame.as_mut_ptr(), depth as u8) };
        core::hint::black_box(&frame);
        recurse_until_stack_fault(depth.wrapping_add(1))
    }

    #[inline(never)]
    fn fault_null_pointer() -> ! {
        // A raw instruction is used here instead of Rust's pointer intrinsics:
        // those intrinsics require a valid pointer even in a deliberate fault
        // test, and may trap before reaching the installed signal handler.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!(
                "mov byte ptr [{address}], 0",
                address = in(reg) 0usize,
                options(nostack, preserves_flags)
            );
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "str wzr, [x0]",
                in("x0") 0usize,
                options(nostack, preserves_flags)
            );
        }
        loop {
            core::hint::spin_loop();
        }
    }
}
