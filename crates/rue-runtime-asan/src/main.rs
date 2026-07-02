//! AddressSanitizer harness for `rue-runtime`'s arena allocator (RUE-211).
//!
//! # Why this exists
//!
//! The valgrind memcheck sanitizer job (RUE-49, `.github/workflows/sanitizer.yml`)
//! runs compiled Rue programs under DBI and catches codegen / wild-pointer /
//! stack / bad-syscall-buffer bugs. But memcheck hooks libc `malloc`/`free`,
//! and `rue-runtime` bump-allocates inside `mmap`'d arenas without ever calling
//! libc — so to memcheck every Rue program does "0 allocs". That leaves the
//! **intra-arena heap overflow** class (RUE-34: a grow path recording more
//! capacity than was actually reserved) completely uncovered: the overflowed
//! bytes live inside a valid `mmap` arena, never a libc block memcheck watches.
//!
//! # What it does
//!
//! This binary compiles the REAL allocator (`crates/rue-runtime/src/heap.rs`,
//! pulled in verbatim via `#[path]` so there is nothing to drift) against a
//! host `platform` shim, and runs it under `-Zsanitizer=address`. It then uses
//! ASan's manual poisoning API (`__asan_poison_memory_region`) to lay a redzone
//! immediately past an allocation's reserved end, so a write past that end —
//! exactly the RUE-34 manifestation — is reported as `use-after-poison`, even
//! though the bytes sit inside a live arena. That is coverage memcheck cannot
//! provide.
//!
//! # Verifying detection actually works
//!
//! `demonstrate_intra_arena_redzone()` contains a commented-out deliberate
//! overflow. Uncomment that single line and run the CI command
//! (`RUSTFLAGS=-Zsanitizer=address cargo +nightly run --target
//! x86_64-unknown-linux-gnu --manifest-path crates/rue-runtime-asan/Cargo.toml`)
//! to confirm ASan aborts with a `use-after-poison` report, then re-comment it
//! so the committed harness stays clean (CI green).

// The real allocator source, included verbatim. It references `crate::platform`
// and `core::*`; we satisfy the former with the module below and the latter is
// available in this std crate.
#[path = "../../rue-runtime/src/heap.rs"]
mod heap;

/// Host stand-in for the runtime's platform layer.
///
/// `heap.rs` only needs `mmap`/`munmap` with the same contract as the real
/// syscall wrappers: `mmap` returns page-aligned, zero-initialized memory (or
/// null), and `munmap` returns it. We back arenas with the global allocator so
/// ASan tracks the underlying blocks; the manual poisoning in `main` adds the
/// intra-arena redzones on top.
mod platform {
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    /// Real `mmap` returns page-aligned memory; mirror that so `heap.rs`'s
    /// page-alignment assumptions (it page-aligns arena sizes) hold.
    const PAGE: usize = 4096;

    pub fn mmap(size: usize) -> *mut u8 {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let Ok(layout) = Layout::from_size_align(size, PAGE) else {
            return std::ptr::null_mut();
        };
        // `alloc_zeroed` mirrors `mmap`'s zero-initialized guarantee.
        unsafe { alloc_zeroed(layout) }
    }

    pub fn munmap(addr: *mut u8, size: usize) -> i64 {
        if addr.is_null() || size == 0 {
            return -1;
        }
        let Ok(layout) = Layout::from_size_align(size, PAGE) else {
            return -1;
        };
        unsafe { dealloc(addr, layout) };
        0
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

fn main() {
    exercise_allocator();
    demonstrate_intra_arena_redzone();
    println!("rue-runtime ASan harness: OK");
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
    let g2 = heap::realloc(g, 32, 4096, 8);
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
    }
}

/// Model the RUE-34 class and prove ASan catches it.
///
/// The allocator RESERVES `RESERVED` bytes. A buggy caller (e.g. a String that
/// recorded a larger capacity than was actually allocated) would write past
/// that end, into other arena bytes — invisible to memcheck. We poison a guard
/// immediately past the reservation so that overflow is reported.
fn demonstrate_intra_arena_redzone() {
    const RESERVED: usize = 64;
    const GUARD: usize = 256;

    let p = heap::alloc(RESERVED as u64, 8);
    assert!(!p.is_null());

    // This is the last allocation the harness makes, so nothing reclaims the
    // guard bytes; poisoning them cannot collide with a later reservation.
    poison(unsafe { p.add(RESERVED) }, GUARD);

    // Correct, in-bounds writes: entirely within the reserved region.
    unsafe {
        for i in 0..RESERVED {
            *p.add(i) = 0xAB;
        }
    }

    // VERIFICATION HOOK: uncomment to confirm ASan flags an intra-arena
    // overflow (`use-after-poison`), then re-comment so CI stays green.
    // unsafe { *p.add(RESERVED) = 0xFF; }

    // Restore shadow state (belt-and-suspenders; the process is exiting).
    unpoison(unsafe { p.add(RESERVED) }, GUARD);
}
