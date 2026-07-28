//! AddressSanitizer harness for Rue's recycling allocator (RUE-211, RUE-560).
//!
//! # Why this exists
//!
//! The Valgrind memcheck CI job (RUE-49, `.github/workflows/ci.yml`)
//! runs compiled Rue programs under DBI and catches codegen / wild-pointer /
//! stack / bad-syscall-buffer bugs. But memcheck hooks libc `malloc`/`free`,
//! and Rue allocates directly inside `mmap`'d arenas without ever calling
//! libc — so to memcheck every Rue program does "0 allocs". That leaves the
//! **intra-arena heap overflow** class (RUE-34: a grow path recording more
//! capacity than was actually reserved) completely uncovered: the overflowed
//! bytes live inside a valid `mmap` arena, never a libc block memcheck watches.
//!
//! # What it does
//!
//! This binary uses the same `rue-allocator` crate as `rue-runtime` against a
//! host page mapper and runs it under `-Zsanitizer=address`. Every
//! allocation the harness makes goes through [`guarded_alloc`], which reserves
//! a redzone immediately past the usable block and poisons it with ASan's
//! manual API (`__asan_poison_memory_region`). A write past an allocation's
//! end — exactly the RUE-34 manifestation — then lands in poisoned shadow and
//! is reported as `use-after-poison`, even though the bytes sit inside a live
//! arena. That is coverage memcheck cannot provide.
//!
//! # The instrumentation is self-verifying (RUE-560)
//!
//! Poisoning that silently stops working would leave the job green while
//! covering nothing — the exact gap RUE-560 documented (the old harness
//! poisoned one artificial final guard and left its detection write commented
//! out, so nothing proved the redzones fire). This harness instead runs
//! **negative controls in child processes** and *requires* each to abort under
//! ASan (see [`require_negative_controls_fail`]). If the redzone instrumentation
//! regresses, a control exits 0, the parent's assertion fails, and the whole
//! job goes red. Coverage is therefore load-bearing, not decorative.
//!
//! A negative control is a deliberate one-byte out-of-bounds write into this
//! harness's own poisoned redzone whose sole purpose is to make ASan fault; it
//! exercises no external input and touches no memory outside the harness.
//!
//! # Controls are judged on their evidence, not their exit code (RUE-1190)
//!
//! "The child failed" is not the same claim as "the redzone caught the
//! overrun". A control child that panicked before its deliberate write, or
//! that aborted on some unrelated sanitizer report, exits nonzero too — so
//! crediting any nonzero exit reopens the RUE-560 gap one level up: coverage
//! could rot while the job stayed green. Each child therefore announces
//! [`CONTROL_REACHED`] on stderr immediately before its overrun, and the
//! parent requires that marker plus exactly one `use-after-poison` report
//! before crediting the control (see [`control_verdict`]).
//!
//! The parent also captures each child's output instead of letting it stream
//! into the job log. Two expected ASan reports printed verbatim, with nothing
//! marking them as deliberate, made a passing run indistinguishable from a
//! catastrophic failure. A healthy run now prints one line per control, and
//! the captured report is dumped only when a control fails to behave.

use std::process::{Command, ExitStatus};

/// Environment variable that puts the harness into negative-control (self-test)
/// mode. Set by [`require_negative_controls_fail`] when it re-execs this binary
/// as a child; the child performs one deliberate poisoned-redzone overrun and
/// is expected to abort under ASan.
const SELFTEST_ENV: &str = "RUE_ASAN_SELFTEST";

/// Announced on stderr by a negative-control child immediately before its
/// deliberate overrun, and required by [`control_verdict`]. A child that dies
/// without printing this failed somewhere earlier, so its nonzero exit proves
/// nothing about the redzones (RUE-1190).
const CONTROL_REACHED: &str = "rue-asan negative control reached its overrun:";

/// Prefix of an AddressSanitizer report line, used to count the reports a
/// negative-control child produced.
const ASAN_ERROR: &str = "ERROR: AddressSanitizer:";

/// The one report a negative control may produce: a write into the manually
/// poisoned redzone past a live allocation's usable end.
const EXPECTED_REPORT: &str = "use-after-poison";

/// The negative controls, each a distinct route to the same overrun class.
const CONTROLS: [&str; 2] = ["raw-overflow", "growth-overflow"];

/// Bytes of poisoned redzone placed past every guarded allocation's usable end.
/// A whole number of 8-byte ASan shadow granules so the poison covers complete
/// granules (partial-granule poisoning cannot flag a write in a granule that
/// also holds valid bytes).
const REDZONE: usize = 32;

/// The real allocator with a host-backed mapper so ASan tracks every underlying
/// arena or dedicated mapping.
mod heap {
    use rue_allocator::{Allocator, PAGE_SIZE, PageMapper};
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    struct HostPageMapper;

    // SAFETY: std's allocator returns exclusive page-aligned blocks for the
    // requested layouts and supports concurrent allocation and deallocation.
    unsafe impl PageMapper for HostPageMapper {
        fn map(size: usize) -> *mut u8 {
            let Ok(layout) = Layout::from_size_align(size, PAGE_SIZE) else {
                return std::ptr::null_mut();
            };
            // SAFETY: the allocator supplies a valid, nonzero mapping size.
            unsafe { alloc_zeroed(layout) }
        }

        unsafe fn unmap(pointer: *mut u8, size: usize) {
            let layout = Layout::from_size_align(size, PAGE_SIZE)
                .expect("allocator supplied an invalid mapping layout");
            // SAFETY: inherited from PageMapper::unmap's contract.
            unsafe { dealloc(pointer, layout) };
        }
    }

    static ALLOCATOR: Allocator<HostPageMapper> = Allocator::new();

    pub fn alloc(size: u64, align: u64) -> *mut u8 {
        ALLOCATOR.allocate(size, align)
    }

    pub unsafe fn free(pointer: *mut u8, size: u64, align: u64) {
        // SAFETY: inherited from this function's caller contract.
        unsafe { ALLOCATOR.deallocate(pointer, size, align) };
    }

    pub unsafe fn realloc(pointer: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
        // SAFETY: inherited from this function's caller contract.
        unsafe { ALLOCATOR.reallocate(pointer, old_size, new_size, align) }
    }
}

// AddressSanitizer manual poisoning API. These symbols are provided by the
// compiler-rt asan runtime that rustc links in under `-Zsanitizer=address`.
unsafe extern "C" {
    fn __asan_poison_memory_region(addr: *const core::ffi::c_void, size: usize);
    fn __asan_unpoison_memory_region(addr: *const core::ffi::c_void, size: usize);
}

/// Mark `[addr, addr+size)` as poisoned: any access reports `use-after-poison`.
fn poison(addr: *const u8, size: usize) {
    unsafe { __asan_poison_memory_region(addr.cast(), size) };
}

/// Clear poisoning on `[addr, addr+size)`.
fn unpoison(addr: *const u8, size: usize) {
    unsafe { __asan_unpoison_memory_region(addr.cast(), size) };
}

/// Round `n` up to the next multiple of 8 (the ASan shadow granularity).
fn align_up_8(n: usize) -> usize {
    (n + 7) & !7
}

/// A guarded allocation: a real arena block whose usable end is followed by a
/// poisoned redzone.
struct Guarded {
    /// Pointer to the usable region (`[ptr, ptr+size)`).
    ptr: *mut u8,
    /// Usable bytes the caller may touch.
    size: usize,
    /// Start of the poisoned redzone, kept so it can be cleared on drop.
    redzone: *mut u8,
    /// Complete allocation layout, including the redzone.
    reserved: usize,
    align: usize,
}

/// Allocate `size` usable bytes through the REAL allocator with a poisoned
/// redzone immediately past the usable end (RUE-560).
///
/// The block reserved from the arena is `align_up_8(size) + REDZONE`; the
/// caller sees only the first `size` bytes. The redzone begins at the
/// 8-aligned boundary at or after `ptr+size` so it covers whole shadow
/// granules. The allocator reserves the full block including that redzone, so
/// poisoning it cannot collide with another live allocation.
fn guarded_alloc(size: usize, align: usize) -> Guarded {
    assert!(size > 0);
    let usable = align_up_8(size);
    let total = usable + REDZONE;
    let ptr = heap::alloc(total as u64, align as u64);
    assert!(!ptr.is_null(), "guarded_alloc({size}, {align}) OOM");
    let redzone = unsafe { ptr.add(usable) };
    poison(redzone, REDZONE);
    Guarded {
        ptr,
        size,
        redzone,
        reserved: total,
        align,
    }
}

impl Guarded {
    /// Fill the usable region with `byte`, then read it back — strictly
    /// in-bounds, so a correct harness run produces no ASan report.
    fn write_and_verify(&self, byte: u8) {
        unsafe {
            for i in 0..self.size {
                *self.ptr.add(i) = byte;
            }
            for i in 0..self.size {
                assert_eq!(*self.ptr.add(i), byte, "byte {i} readback mismatch");
            }
        }
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        // Clear the shadow before returning the complete block to its free
        // list; a later allocation may immediately recycle this storage.
        unpoison(self.redzone, REDZONE);
        // SAFETY: ptr is live with the complete reserved layout.
        unsafe { heap::free(self.ptr, self.reserved as u64, self.align as u64) };
    }
}

fn main() {
    // Negative-control mode (RUE-560): a child process re-execed by
    // `require_negative_controls_fail` runs exactly one deliberate
    // poisoned-redzone overrun and is expected to abort under ASan.
    if let Ok(mode) = std::env::var(SELFTEST_ENV) {
        run_negative_control(&mode);
        // A working redzone aborts inside `run_negative_control` (nonzero
        // exit). Reaching here means the poisoned write was NOT caught, so the
        // overrun landed in valid arena bytes — the RUE-560 gap. Exit 0
        // (success) on purpose: the parent requires this child to FAIL, so a
        // clean exit trips its assertion and turns the whole job red.
        eprintln!(
            "negative control '{mode}' completed WITHOUT an ASan report — \
             redzone instrumentation is not catching the overrun"
        );
        return;
    }

    exercise_allocator();
    exercise_guarded_allocations();
    exercise_container_growth();
    require_negative_controls_fail();
    println!(
        "rue-runtime ASan harness: OK (allocator exercises clean, {} negative controls proven)",
        CONTROLS.len()
    );
}

/// Drive the real allocator through its main paths with strictly in-bounds
/// accesses. No manual poisoning here, so `realloc`'s internal grow-and-copy
/// runs without a false positive; ASan still watches every access against the
/// underlying arena blocks.
fn exercise_allocator() {
    // Basic alloc, fully written and read back.
    let p = heap::alloc(64, 8);
    assert!(!p.is_null());
    unsafe {
        for i in 0..64 {
            *p.add(i) = i as u8;
        }
        for i in 0..64 {
            assert_eq!(*p.add(i), i as u8);
        }
    }

    // Many distinct allocations, each written.
    let mut ptrs = [core::ptr::null_mut::<u8>(); 32];
    for (i, slot) in ptrs.iter_mut().enumerate() {
        let q = heap::alloc(128, 8);
        assert!(!q.is_null());
        unsafe { *q = i as u8 };
        *slot = q;
    }
    for (i, &q) in ptrs.iter().enumerate() {
        unsafe { assert_eq!(*q, i as u8) };
    }

    // realloc grow: exercises the real fresh-alloc + copy_nonoverlapping path,
    // then writes the whole grown region. Data before the grow must survive.
    let g = heap::alloc(32, 8);
    assert!(!g.is_null());
    unsafe {
        for i in 0..32 {
            *g.add(i) = (i as u8).wrapping_mul(3);
        }
    }
    // SAFETY: g is live and readable for 32 bytes.
    let g2 = unsafe { heap::realloc(g, 32, 4096, 8) };
    assert!(!g2.is_null());
    unsafe {
        for i in 0..32 {
            assert_eq!(*g2.add(i), (i as u8).wrapping_mul(3), "grow lost byte {i}");
        }
        for i in 0..4096 {
            *g2.add(i) = 0xEE;
        }
    }

    // Large allocation spanning more than one default arena.
    const BIG: usize = 200 * 1024;
    let big = heap::alloc(BIG as u64, 8);
    assert!(!big.is_null());
    unsafe {
        *big = 1;
        *big.add(BIG - 1) = 2;
        assert_eq!(*big, 1);
        assert_eq!(*big.add(BIG - 1), 2);

        heap::free(p, 64, 8);
        for q in ptrs {
            heap::free(q, 128, 8);
        }
        heap::free(g2, 4096, 8);
        heap::free(big, BIG as u64, 8);
    }
}

/// Exercise per-allocation redzones on the real allocator with strictly
/// in-bounds writes (RUE-560). Every block carries a poisoned redzone; because
/// all accesses stay within the usable region, a working harness produces no
/// ASan report. The negative controls prove the redzones would fire on an
/// overrun.
fn exercise_guarded_allocations() {
    // A spread of usable sizes, including non-8-multiples (redzone still lands
    // on an 8-aligned boundary), across several alignments.
    for &(size, align) in &[(1usize, 1usize), (7, 8), (64, 8), (100, 16), (4096, 8)] {
        let g = guarded_alloc(size, align);
        assert_eq!(g.ptr as usize % align, 0, "bad alignment for align={align}");
        g.write_and_verify(0xAB);
    }

    // Many live guarded allocations at once: each reserves its own redzone, so
    // one block's poison never overlaps another block's usable region.
    let blocks: Vec<Guarded> = (0..48).map(|i| guarded_alloc(24 + i, 8)).collect();
    for g in &blocks {
        g.write_and_verify(0xCD);
    }
    drop(blocks);
}

/// A minimal growable byte buffer over the real allocator, modeling the
/// container grow path RUE-34 named — with a poisoned redzone tracking the
/// *real* reservation, so a write past the truly-allocated end is caught even
/// if a length/capacity bookkeeping bug lets it through (RUE-560).
struct GuardedVec {
    buf: Guarded,
    len: usize,
    cap: usize,
}

impl GuardedVec {
    fn with_capacity(cap: usize) -> Self {
        Self {
            buf: guarded_alloc(cap, 8),
            len: 0,
            cap,
        }
    }

    /// Push one byte, growing (fresh guarded alloc + copy, mirroring
    /// `heap::realloc`'s grow path) when full.
    fn push(&mut self, byte: u8) {
        if self.len == self.cap {
            let new_cap = self.cap * 2;
            let new_buf = guarded_alloc(new_cap, 8);
            unsafe {
                core::ptr::copy_nonoverlapping(self.buf.ptr, new_buf.ptr, self.len);
            }
            self.buf = new_buf;
            self.cap = new_cap;
        }
        unsafe { *self.buf.ptr.add(self.len) = byte };
        self.len += 1;
    }

    fn get(&self, i: usize) -> u8 {
        assert!(i < self.len);
        unsafe { *self.buf.ptr.add(i) }
    }
}

/// Exercise a container-like grow loop end to end, strictly in-bounds
/// (RUE-560). Pushes enough bytes to force several reallocations and verifies
/// every byte survives each grow — the real-world path (String/ArrayBuf push)
/// the RUE-34 overflow class lives on.
fn exercise_container_growth() {
    let mut v = GuardedVec::with_capacity(8);
    for i in 0..1000usize {
        v.push((i & 0xFF) as u8);
    }
    for i in 0..1000usize {
        assert_eq!(v.get(i), (i & 0xFF) as u8, "grow lost byte {i}");
    }
}

/// Re-exec this binary once per negative control and REQUIRE each child to fail
/// under ASan (RUE-560). This is what makes the redzone coverage load-bearing:
/// if the poisoning ever silently stops working, a control exits 0, the
/// assertion here fires, and the whole harness (and CI job) goes red.
///
/// The child inherits `RUSTFLAGS`-baked ASan instrumentation and `ASAN_OPTIONS`
/// from the environment; it runs [`run_negative_control`] and is expected to
/// abort with a `use-after-poison` report (nonzero exit).
///
/// Each child's output is captured rather than inherited, so the expected ASan
/// reports do not stream into the job log and masquerade as a failure. It is
/// also the evidence [`control_verdict`] judges: a child is credited only when
/// it reached its deliberate overrun and died on exactly that report.
fn require_negative_controls_fail() {
    let exe = std::env::current_exe().expect("current_exe");
    for mode in CONTROLS {
        let output = Command::new(&exe)
            .env(SELFTEST_ENV, mode)
            .output()
            .expect("spawn negative-control child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Err(reason) = control_verdict(mode, output.status, &stderr) {
            panic!(
                "NEGATIVE CONTROL '{mode}' DID NOT PROVE ASan CAUGHT ITS OVERRUN: \
                 {reason}. The redzone instrumentation is not demonstrably \
                 catching intra-arena overruns — the RUE-560 coverage gap has \
                 regressed.\n--- captured child output ---\n{stderr}\
                 --- end captured child output ---"
            );
        }
        println!("negative control '{mode}': ASan reported {EXPECTED_REPORT} as required");
    }
}

/// Decide whether a negative-control child proved the redzone fired.
///
/// Only one outcome counts: the child announced [`CONTROL_REACHED`], then died
/// on exactly one `use-after-poison` report. Every other nonzero exit — a panic
/// before the overrun, a different sanitizer report, an extra report from
/// somewhere else in the child — is a failure to demonstrate coverage, not
/// evidence of it (RUE-1190).
fn control_verdict(mode: &str, status: ExitStatus, stderr: &str) -> Result<(), String> {
    if status.success() {
        return Err(format!(
            "the child exited successfully ({status}), so the deliberate \
             overrun landed in valid arena bytes instead of poisoned shadow"
        ));
    }
    if !stderr.contains(&format!("{CONTROL_REACHED} {mode}")) {
        return Err(format!(
            "the child exited {status} before announcing that it reached the \
             deliberate overrun, so its failure has some other cause"
        ));
    }
    let reports = stderr.matches(ASAN_ERROR).count();
    if reports != 1 {
        return Err(format!(
            "expected exactly one AddressSanitizer report, but the child \
             produced {reports}"
        ));
    }
    if !stderr.contains(&format!("{ASAN_ERROR} {EXPECTED_REPORT}")) {
        return Err(format!(
            "the child's AddressSanitizer report was not `{EXPECTED_REPORT}`, \
             so the redzone is not what stopped it"
        ));
    }
    Ok(())
}

/// Perform exactly one deliberate poisoned-redzone overrun for the named
/// control. Under a working harness ASan reports `use-after-poison` and aborts
/// the process here; this function does not return on success.
fn run_negative_control(mode: &str) {
    match mode {
        // A raw allocation overrun: write one byte past the usable end, into
        // the poisoned redzone. This is the `*p.add(64) = 0xFF` in the issue's
        // repro, but against the guarded allocator every allocation carries.
        "raw-overflow" => {
            let g = guarded_alloc(64, 8);
            g.write_and_verify(0xAB); // in-bounds, fine
            announce_reached(mode);
            // One byte past the usable end -> poisoned -> ASan aborts.
            unsafe { *g.ptr.add(g.size) = 0xFF };
        }
        // The RUE-34 class directly: a container whose bookkeeping thinks it
        // has one more byte of capacity than was really reserved writes at the
        // recorded end, past the real allocation, into the redzone.
        "growth-overflow" => {
            let mut v = GuardedVec::with_capacity(8);
            for i in 0..8usize {
                v.push(i as u8);
            }
            announce_reached(mode);
            // `v` is full (len == cap == 8). Simulate a grow-path bug that
            // recorded capacity without reserving it: write at the real end
            // WITHOUT growing. That byte lands in the poisoned redzone.
            unsafe { *v.buf.ptr.add(v.cap) = 0xFF };
        }
        other => {
            eprintln!("unknown negative-control mode: {other}");
            std::process::exit(3);
        }
    }
}

/// Tell the parent this child got as far as its deliberate overrun, so a
/// following ASan report can be attributed to that write (RUE-1190). Printed to
/// stderr, which is unbuffered, so it survives the abort that follows.
fn announce_reached(mode: &str) {
    eprintln!("{CONTROL_REACHED} {mode}");
}
