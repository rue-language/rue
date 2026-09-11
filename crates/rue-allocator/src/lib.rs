//! Dependency-free allocation policy for Rue runtimes.
//!
//! [`Allocator`] combines power-of-two free lists for small allocations with
//! dedicated mappings for large allocations. It is independent of any runtime
//! ABI or operating-system syscall convention; consumers supply those details
//! through [`PageMapper`].

#![no_std]

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

/// Page size required from [`PageMapper`].
pub const PAGE_SIZE: usize = 4096;

/// Size of the arenas used for small allocations.
pub const ARENA_SIZE: usize = 64 * 1024;

/// Largest power-of-two class served from the arenas.
pub const MAX_SMALL_SIZE: usize = 16 * 1024;

const MIN_CLASS_SIZE: usize = core::mem::size_of::<*mut u8>();
const MIN_CLASS_SHIFT: usize = MIN_CLASS_SIZE.trailing_zeros() as usize;
const MAX_CLASS_SHIFT: usize = MAX_SMALL_SIZE.trailing_zeros() as usize;
const CLASS_COUNT: usize = MAX_CLASS_SHIFT - MIN_CLASS_SHIFT + 1;

const _: () = {
    assert!(core::mem::size_of::<*mut u8>() == 8);
    assert!(MIN_CLASS_SIZE.is_power_of_two());
    assert!(MAX_SMALL_SIZE.is_power_of_two());
    assert!(ARENA_SIZE.is_multiple_of(PAGE_SIZE));
};

/// Supplies page mappings to an [`Allocator`].
///
/// Implementations normally forward to anonymous `mmap` and `munmap` syscalls,
/// but tests and instrumentation can provide host-allocator-backed mappings.
///
/// # Safety
///
/// Implementations must uphold the mapping validity, alignment, lifetime, and
/// concurrency contracts below. [`Allocator`] dereferences returned mappings
/// from safe methods and may call the mapper concurrently.
pub unsafe trait PageMapper {
    /// Decide whether an allocation attempt may proceed.
    ///
    /// Production mappers normally use the default. The hook lets a runtime
    /// inject deterministic allocation failure in tests without adding mutable
    /// failure state to the allocator or changing its allocation paths.
    fn allocation_permitted() -> bool {
        true
    }

    /// Map `size` readable and writable bytes.
    ///
    /// `size` is nonzero and a multiple of [`PAGE_SIZE`]. On success, the
    /// returned pointer must be aligned to [`PAGE_SIZE`] and remain valid until
    /// the matching [`unmap`](Self::unmap). Return null on failure.
    fn map(size: usize) -> *mut u8;

    /// Release a mapping previously returned by [`map`](Self::map).
    ///
    /// # Safety
    ///
    /// `pointer` and `size` must describe one currently live mapping returned
    /// by this mapper, and no access to that mapping may occur after the call.
    unsafe fn unmap(pointer: *mut u8, size: usize);
}

/// Header stored at the beginning of every small-allocation arena.
#[repr(C)]
struct ArenaHeader {
    next: *mut ArenaHeader,
    size: usize,
    offset: usize,
}

const _: () = {
    assert!(core::mem::size_of::<ArenaHeader>() == 24);
    assert!(core::mem::align_of::<ArenaHeader>() == 8);
};

/// Link stored directly in a freed small block.
#[repr(C)]
struct FreeBlock {
    next: *mut FreeBlock,
}

struct AllocatorState {
    current_arena: *mut ArenaHeader,
    free_lists: [*mut FreeBlock; CLASS_COUNT],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SmallClass {
    index: usize,
    block_size: usize,
    block_align: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllocationKind {
    Small(SmallClass),
    Direct { mapping_size: usize },
}

#[cfg(debug_assertions)]
const DEBUG_HEADER_MAGIC: usize = 0x5255_4541_4c4c_4f43;

#[cfg(debug_assertions)]
#[repr(C)]
struct AllocationHeader {
    magic: usize,
    size: u64,
    align: u64,
    raw_pointer: *mut u8,
    mapping_size: usize,
    kind: usize,
}

#[cfg(debug_assertions)]
const _: () = {
    assert!(core::mem::size_of::<AllocationHeader>() == 48);
    assert!(core::mem::align_of::<AllocationHeader>() == 8);
};

#[cfg(debug_assertions)]
const DIRECT_KIND: usize = usize::MAX;

/// A recycling arena allocator using mappings supplied by `M`.
///
/// Small requests are rounded to power-of-two classes from 8 bytes through
/// 16 KiB. Freed blocks are linked into per-class intrusive free lists. Larger
/// requests receive their own page-rounded mapping and return it immediately
/// on deallocation. Arenas are retained for the allocator's lifetime, so small
/// allocation memory follows a high-water mark.
///
/// One spin lock serializes the small-allocation state. Mapping implementations
/// must independently support concurrent calls because dedicated mappings do
/// not acquire that lock.
pub struct Allocator<M: PageMapper> {
    lock: AtomicBool,
    state: UnsafeCell<AllocatorState>,
    mapper: PhantomData<fn() -> M>,
}

// SAFETY: `lock` serializes every access to `state`, and PageMapper's contract
// requires its static mapping operations to support concurrent calls.
unsafe impl<M: PageMapper> Sync for Allocator<M> {}

impl<M: PageMapper> Default for Allocator<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: PageMapper> Allocator<M> {
    /// Construct an empty allocator without mapping any memory.
    pub const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            state: UnsafeCell::new(AllocatorState {
                current_arena: ptr::null_mut(),
                free_lists: [ptr::null_mut(); CLASS_COUNT],
            }),
            mapper: PhantomData,
        }
    }

    /// Allocate `size` bytes with at least `align` alignment.
    ///
    /// Returns null for a zero size, an unsupported layout, integer overflow,
    /// or mapping failure. Successful storage is uninitialized.
    pub fn allocate(&self, size: u64, align: u64) -> *mut u8 {
        if !M::allocation_permitted() {
            return ptr::null_mut();
        }

        let Some(kind) = classify(size, align) else {
            return ptr::null_mut();
        };

        match kind {
            AllocationKind::Small(class) => self.allocate_small(class, size, align),
            AllocationKind::Direct { mapping_size } => {
                #[cfg(debug_assertions)]
                let payload_offset = checked_align_up(
                    core::mem::size_of::<AllocationHeader>(),
                    match usize::try_from(align) {
                        Ok(align) => align,
                        Err(_) => unreachable!(),
                    },
                );
                #[cfg(debug_assertions)]
                let Some(payload_offset) = payload_offset else {
                    return ptr::null_mut();
                };
                #[cfg(debug_assertions)]
                let physical_mapping_size = mapping_size
                    .checked_add(payload_offset)
                    .and_then(|size| size.checked_next_multiple_of(PAGE_SIZE));
                #[cfg(debug_assertions)]
                let Some(mapping_size_for_mapper) = physical_mapping_size else {
                    return ptr::null_mut();
                };
                #[cfg(not(debug_assertions))]
                let mapping_size_for_mapper = mapping_size;
                let raw_pointer = M::map(mapping_size_for_mapper);
                if raw_pointer.is_null() {
                    return ptr::null_mut();
                }
                #[cfg(debug_assertions)]
                {
                    // SAFETY: the mapping is page-aligned and the checked
                    // offset leaves the complete header and allocation in it.
                    let pointer = unsafe { raw_pointer.add(payload_offset) };
                    // SAFETY: pointer is in the fresh mapping; the padded
                    // prefix keeps its header aligned and before the payload.
                    unsafe {
                        write_allocation_header(
                            pointer,
                            size,
                            align,
                            raw_pointer,
                            mapping_size_for_mapper,
                            DIRECT_KIND,
                        );
                    }
                    pointer
                }
                #[cfg(not(debug_assertions))]
                {
                    raw_pointer
                }
            }
        }
    }

    /// Release an allocation described by its original layout.
    ///
    /// A null pointer is always accepted and has no effect. In debug builds,
    /// the exact layout recorded at allocation time is checked before the
    /// block is classified or returned to a free list. A mismatch panics;
    /// arbitrary pointers and double frees remain outside this contract.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a live allocation returned by this
    /// allocator for exactly `size` and `align`. It must not be used afterward.
    /// Debug builds validate the exact layout of a live allocation before
    /// changing allocator state; pointer validity and ownership remain the
    /// caller's responsibility.
    pub unsafe fn deallocate(&self, pointer: *mut u8, size: u64, align: u64) {
        if pointer.is_null() {
            return;
        }

        #[cfg(debug_assertions)]
        {
            let record = unsafe { validate_allocation_header(pointer, size, align) };
            if record.kind == DIRECT_KIND {
                // SAFETY: the checked header came from this allocator and
                // records the complete mapping returned by M::map.
                unsafe { M::unmap(record.raw_pointer, record.mapping_size) };
                return;
            }
            // SAFETY: the caller owns this live block, and its header records
            // the small class that allocated it.
            unsafe { self.release_small(pointer, record.kind) };
        }

        #[cfg(not(debug_assertions))]
        {
            let Some(kind) = classify(size, align) else {
                return;
            };

            match kind {
                AllocationKind::Small(class) => {
                    // SAFETY: the caller supplied this live block's exact layout.
                    unsafe { self.release_small(pointer, class.index) };
                }
                AllocationKind::Direct { mapping_size } => {
                    // SAFETY: direct allocations return the mapping base, and the
                    // caller supplied the exact layout used to derive its size.
                    unsafe { M::unmap(pointer, mapping_size) };
                }
            }
        }
    }

    /// Return a live small block to its class's free list.
    ///
    /// # Safety
    ///
    /// `pointer` must be a live block owned by this allocator in `class_index`.
    /// The caller retires the allocation and must not access it afterward.
    unsafe fn release_small(&self, pointer: *mut u8, class_index: usize) {
        let _guard = self.lock();
        // SAFETY: the lock grants exclusive access to the allocator state, and
        // callers provide a live block in the recorded class.
        unsafe {
            let state = &mut *self.state.get();
            let block = pointer.cast::<FreeBlock>();
            (*block).next = state.free_lists[class_index];
            state.free_lists[class_index] = block;
        }
    }

    /// Try to relabel a live allocation as `new_size` bytes without moving it.
    ///
    /// Returns `true` when the block already satisfies the new layout — the
    /// same size class, or the same page-rounded mapping — in which case the
    /// caller may treat the memory at `pointer` as `new_size` bytes and must
    /// hand `new_size` back at deallocation. Returns `false` without touching
    /// anything when the new layout needs different storage; the caller keeps
    /// the old `(old_size, align)` layout and can fall back to
    /// [`Allocator::reallocate`].
    ///
    /// This is the in-place-only half of `reallocate`: it never allocates,
    /// never copies, and never frees, so a container that manages its own copy
    /// can grow where it stands whenever the allocator has room.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a live allocation returned by this
    /// allocator for exactly `old_size` and `align`.
    pub unsafe fn resize_in_place(
        &self,
        pointer: *mut u8,
        old_size: u64,
        new_size: u64,
        align: u64,
    ) -> bool {
        // A null block owns no storage and a zero new size describes no
        // allocation at all; neither can be satisfied in place.
        if pointer.is_null() {
            return false;
        }
        #[cfg(debug_assertions)]
        let record = unsafe { validate_allocation_header(pointer, old_size, align) };
        if new_size == 0 {
            return false;
        }
        let (Some(old_kind), Some(new_kind)) =
            (classify(old_size, align), classify(new_size, align))
        else {
            return false;
        };
        #[cfg(debug_assertions)]
        assert!(record.kind == allocation_kind_key(old_kind));
        if old_kind == new_kind {
            #[cfg(debug_assertions)]
            unsafe {
                update_allocation_size(pointer, new_size)
            };
            return true;
        }
        false
    }

    /// Resize an allocation while preserving its initialized prefix.
    ///
    /// If the old and new layouts use the same size class or page-rounded
    /// mapping, the pointer is returned unchanged. Otherwise this allocates a
    /// replacement, copies `min(old_size, new_size)` bytes, and releases the
    /// old allocation. Allocation failure returns null and leaves the old block
    /// live and unchanged.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a live allocation returned by this
    /// allocator for exactly `old_size` and `align`, readable for `old_size`
    /// bytes. After a successful resize, it must no longer be used through the
    /// old pointer unless the returned pointer is equal to it.
    pub unsafe fn reallocate(
        &self,
        pointer: *mut u8,
        old_size: u64,
        new_size: u64,
        align: u64,
    ) -> *mut u8 {
        if pointer.is_null() {
            return self.allocate(new_size, align);
        }
        #[cfg(debug_assertions)]
        let record = unsafe { validate_allocation_header(pointer, old_size, align) };
        if new_size == 0 {
            // SAFETY: inherited from this function's caller contract.
            unsafe { self.deallocate(pointer, old_size, align) };
            return ptr::null_mut();
        }

        let Some(old_kind) = classify(old_size, align) else {
            return ptr::null_mut();
        };
        let Some(new_kind) = classify(new_size, align) else {
            return ptr::null_mut();
        };

        #[cfg(debug_assertions)]
        assert!(record.kind == allocation_kind_key(old_kind));

        if old_kind == new_kind {
            #[cfg(debug_assertions)]
            unsafe {
                update_allocation_size(pointer, new_size)
            };
            return pointer;
        }

        let replacement = self.allocate(new_size, align);
        if replacement.is_null() {
            return ptr::null_mut();
        }

        let copy_size = old_size.min(new_size) as usize;
        // SAFETY: the two live allocations do not overlap, the old allocation
        // is readable for old_size, and the new one is writable for new_size.
        unsafe { ptr::copy_nonoverlapping(pointer, replacement, copy_size) };
        // SAFETY: the successful copy retires the old allocation.
        unsafe { self.deallocate(pointer, old_size, align) };
        replacement
    }

    fn allocate_small(&self, class: SmallClass, size: u64, align: u64) -> *mut u8 {
        let _guard = self.lock();
        // SAFETY: the lock grants exclusive access to the allocator state.
        let state = unsafe { &mut *self.state.get() };

        let free = state.free_lists[class.index];
        if !free.is_null() {
            // SAFETY: every list entry is a class-sized, properly aligned freed
            // block whose first word contains a FreeBlock link.
            unsafe {
                state.free_lists[class.index] = (*free).next;
                #[cfg(debug_assertions)]
                write_allocation_header(
                    free.cast::<u8>(),
                    size,
                    align,
                    ptr::null_mut(),
                    0,
                    class.index,
                );
            }
            return free.cast::<u8>();
        }

        loop {
            if !state.current_arena.is_null() {
                // SAFETY: current_arena is a live mapping owned by this state.
                if let Some(pointer) =
                    unsafe { allocate_from_arena(state.current_arena, class, size, align) }
                {
                    return pointer;
                }
            }

            let mapping = M::map(ARENA_SIZE);
            if mapping.is_null() {
                return ptr::null_mut();
            }

            let arena = mapping.cast::<ArenaHeader>();
            // SAFETY: M returned an exclusive page-aligned ARENA_SIZE mapping,
            // which has room for the header at its beginning.
            unsafe {
                arena.write(ArenaHeader {
                    next: state.current_arena,
                    size: ARENA_SIZE,
                    offset: core::mem::size_of::<ArenaHeader>(),
                });
            }
            state.current_arena = arena;
        }
    }

    fn lock(&self) -> AllocatorLock<'_, M> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        AllocatorLock { allocator: self }
    }
}

struct AllocatorLock<'a, M: PageMapper> {
    allocator: &'a Allocator<M>,
}

impl<M: PageMapper> Drop for AllocatorLock<'_, M> {
    fn drop(&mut self) {
        self.allocator.lock.store(false, Ordering::Release);
    }
}

fn classify(size: u64, align: u64) -> Option<AllocationKind> {
    if size == 0 || align == 0 || !align.is_power_of_two() || align > PAGE_SIZE as u64 {
        return None;
    }

    let size = usize::try_from(size).ok()?;
    let align = usize::try_from(align).ok()?;
    if size <= MAX_SMALL_SIZE {
        let required = size.max(align).max(MIN_CLASS_SIZE);
        let block_size = required.checked_next_power_of_two()?;
        if block_size <= MAX_SMALL_SIZE {
            let shift = block_size.trailing_zeros() as usize;
            return Some(AllocationKind::Small(SmallClass {
                index: shift - MIN_CLASS_SHIFT,
                block_size,
                block_align: block_size.min(PAGE_SIZE),
            }));
        }
    }

    let mapping_size = size.checked_next_multiple_of(PAGE_SIZE)?;
    Some(AllocationKind::Direct { mapping_size })
}

/// Try to bump one class-sized block from `arena`.
///
/// # Safety
///
/// The caller must hold the allocator lock and `arena` must point to the live
/// current arena owned by that allocator.
unsafe fn allocate_from_arena(
    arena: *mut ArenaHeader,
    class: SmallClass,
    size: u64,
    align: u64,
) -> Option<*mut u8> {
    #[cfg(not(debug_assertions))]
    let _ = (size, align);
    // SAFETY: guaranteed by the caller.
    let header = unsafe { &mut *arena };
    #[cfg(debug_assertions)]
    let pointer_offset = {
        let pointer_offset = checked_align_up(
            header
                .offset
                .checked_add(core::mem::size_of::<AllocationHeader>())?,
            class.block_align,
        )?;
        pointer_offset
    };
    #[cfg(not(debug_assertions))]
    let pointer_offset = checked_align_up(header.offset, class.block_align)?;
    let new_offset = pointer_offset.checked_add(class.block_size)?;
    if new_offset > header.size {
        return None;
    }

    header.offset = new_offset;
    // SAFETY: the checked offsets place the complete block in the arena.
    let pointer = unsafe { arena.cast::<u8>().add(pointer_offset) };
    #[cfg(debug_assertions)]
    unsafe {
        // SAFETY: pointer_offset is within the arena, and the header is
        // immediately before the returned pointer.
        write_allocation_header(pointer, size, align, ptr::null_mut(), 0, class.index);
    }
    Some(pointer)
}

const fn checked_align_up(value: usize, align: usize) -> Option<usize> {
    let Some(added) = value.checked_add(align - 1) else {
        return None;
    };
    Some(added & !(align - 1))
}

#[cfg(debug_assertions)]
unsafe fn write_allocation_header(
    pointer: *mut u8,
    size: u64,
    align: u64,
    raw_pointer: *mut u8,
    mapping_size: usize,
    kind: usize,
) {
    let header = unsafe {
        pointer
            .sub(core::mem::size_of::<AllocationHeader>())
            .cast::<AllocationHeader>()
    };
    // SAFETY: callers place the header immediately before a suitably aligned
    // pointer in storage owned by this allocator.
    unsafe {
        header.write(AllocationHeader {
            magic: DEBUG_HEADER_MAGIC,
            size,
            align,
            raw_pointer,
            mapping_size,
            kind,
        });
    }
}

#[cfg(debug_assertions)]
unsafe fn validate_allocation_header(pointer: *mut u8, size: u64, align: u64) -> AllocationHeader {
    let header = unsafe {
        pointer
            .sub(core::mem::size_of::<AllocationHeader>())
            .cast::<AllocationHeader>()
    };
    // SAFETY: the caller promises a pointer returned by this allocator; the
    // header was written when that allocation was created.
    let record = unsafe { header.read() };
    assert!(record.magic == DEBUG_HEADER_MAGIC);
    assert!(record.size == size);
    assert!(record.align == align);
    record
}

#[cfg(debug_assertions)]
unsafe fn update_allocation_size(pointer: *mut u8, size: u64) {
    let header = unsafe {
        pointer
            .sub(core::mem::size_of::<AllocationHeader>())
            .cast::<AllocationHeader>()
    };
    // SAFETY: the header belongs to this live allocation and remains in place.
    unsafe { (*header).size = size };
}

#[cfg(debug_assertions)]
const fn allocation_kind_key(kind: AllocationKind) -> usize {
    match kind {
        AllocationKind::Small(class) => class.index,
        AllocationKind::Direct { .. } => DIRECT_KIND,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use core::sync::atomic::{AtomicBool, AtomicUsize};
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    struct TestMapper;

    // SAFETY: std's allocator returns exclusive page-aligned blocks for the
    // requested layouts and supports concurrent allocation and deallocation.
    unsafe impl PageMapper for TestMapper {
        fn map(size: usize) -> *mut u8 {
            let layout = Layout::from_size_align(size, PAGE_SIZE).unwrap();
            // SAFETY: layout is nonzero and valid.
            unsafe { alloc_zeroed(layout) }
        }

        unsafe fn unmap(pointer: *mut u8, size: usize) {
            let layout = Layout::from_size_align(size, PAGE_SIZE).unwrap();
            // SAFETY: inherited from PageMapper::unmap's contract.
            unsafe { dealloc(pointer, layout) };
        }
    }

    #[test]
    fn layouts_select_expected_classes_and_mappings() {
        assert_eq!(classify(1, 1), classify(8, 8));
        assert_eq!(classify(17, 1), classify(32, 32));
        assert_eq!(
            classify(MAX_SMALL_SIZE as u64, 8),
            Some(AllocationKind::Small(SmallClass {
                index: CLASS_COUNT - 1,
                block_size: MAX_SMALL_SIZE,
                block_align: PAGE_SIZE,
            }))
        );
        assert_eq!(
            classify(MAX_SMALL_SIZE as u64 + 1, 8),
            Some(AllocationKind::Direct {
                mapping_size: 5 * PAGE_SIZE,
            })
        );
    }

    #[test]
    fn invalid_layouts_are_rejected() {
        assert!(classify(0, 8).is_none());
        assert!(classify(8, 0).is_none());
        assert!(classify(8, 3).is_none());
        assert!(classify(8, (PAGE_SIZE * 2) as u64).is_none());
        assert!(classify(u64::MAX, 1).is_none());
    }

    #[test]
    fn allocations_honor_every_supported_alignment() {
        let allocator = Allocator::<TestMapper>::new();
        for align in [1u64, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
            let pointer = allocator.allocate(64, align);
            assert!(!pointer.is_null());
            assert_eq!(pointer as usize % align as usize, 0);
            // SAFETY: pointer is live with the layout just requested.
            unsafe { allocator.deallocate(pointer, 64, align) };
        }
    }

    #[test]
    fn freed_small_block_is_reused_by_its_class() {
        let allocator = Allocator::<TestMapper>::new();
        let first = allocator.allocate(6000, 8);
        assert!(!first.is_null());
        // SAFETY: first is live with this layout.
        unsafe { allocator.deallocate(first, 6000, 8) };

        let second = allocator.allocate(6000, 8);
        assert_eq!(second, first);
    }

    #[test]
    fn different_small_classes_do_not_share_blocks() {
        let allocator = Allocator::<TestMapper>::new();
        let larger = allocator.allocate(17, 1);
        assert!(!larger.is_null());
        // SAFETY: larger is live with this layout.
        unsafe { allocator.deallocate(larger, 17, 1) };

        let smaller = allocator.allocate(16, 1);
        assert!(!smaller.is_null());
        assert_ne!(smaller, larger);
    }

    #[test]
    fn resize_in_place_accepts_the_same_class_and_refuses_a_different_one() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(65, 8);
        assert!(!pointer.is_null());
        // SAFETY: pointer is live for 65 bytes.
        unsafe { pointer.write(99) };

        // 65 and 120 share the 128-byte class, so the block is relabeled where
        // it stands and its bytes are untouched.
        // SAFETY: pointer is live with layout (65, 8).
        assert!(unsafe { allocator.resize_in_place(pointer, 65, 120, 8) });
        // SAFETY: the block is still live and readable.
        assert_eq!(unsafe { pointer.read() }, 99);

        // A direct mapping needs different storage, so the block cannot grow
        // in place and the old layout stays in force.
        // SAFETY: pointer is live with layout (120, 8).
        assert!(!unsafe { allocator.resize_in_place(pointer, 120, 40_000, 8) });
        // SAFETY: pointer is live with layout (120, 8).
        unsafe { allocator.deallocate(pointer, 120, 8) };
    }

    #[test]
    fn resize_in_place_refuses_a_null_block_and_a_zero_new_size() {
        let allocator = Allocator::<TestMapper>::new();
        // SAFETY: a null block is explicitly accepted and refused.
        assert!(!unsafe { allocator.resize_in_place(ptr::null_mut(), 0, 16, 8) });

        let pointer = allocator.allocate(64, 8);
        assert!(!pointer.is_null());
        // SAFETY: pointer is live with layout (64, 8).
        assert!(!unsafe { allocator.resize_in_place(pointer, 64, 0, 8) });
        // SAFETY: the refusal left the block live and unchanged.
        unsafe { allocator.deallocate(pointer, 64, 8) };
    }

    #[test]
    fn realloc_within_one_class_keeps_the_pointer() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(65, 8);
        assert!(!pointer.is_null());
        unsafe { pointer.write(0x5a) };

        // SAFETY: pointer is live and readable for 65 bytes.
        let grown = unsafe { allocator.reallocate(pointer, 65, 120, 8) };
        assert_eq!(grown, pointer);
        assert_eq!(unsafe { grown.read() }, 0x5a);
        // SAFETY: successful growth relabeled the allocation to this layout.
        unsafe { allocator.deallocate(grown, 120, 8) };
    }

    #[test]
    fn realloc_between_classes_copies_and_recycles() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(32, 8);
        assert!(!pointer.is_null());
        unsafe {
            for index in 0..32 {
                pointer.add(index).write(index as u8);
            }
        }

        // SAFETY: pointer is live and readable for 32 bytes.
        let grown = unsafe { allocator.reallocate(pointer, 32, 128, 8) };
        assert!(!grown.is_null());
        assert_ne!(grown, pointer);
        unsafe {
            for index in 0..32 {
                assert_eq!(grown.add(index).read(), index as u8);
            }
        }

        // The old block was returned to the 32-byte class.
        assert_eq!(allocator.allocate(32, 8), pointer);
    }

    #[test]
    fn direct_allocation_is_usable_and_releasable() {
        let allocator = Allocator::<TestMapper>::new();
        let size = (MAX_SMALL_SIZE + 1) as u64;
        let pointer = allocator.allocate(size, 8);
        assert!(!pointer.is_null());
        unsafe {
            pointer.write(1);
            pointer.add(size as usize - 1).write(2);
            assert_eq!(pointer.read(), 1);
            assert_eq!(pointer.add(size as usize - 1).read(), 2);
            allocator.deallocate(pointer, size, 8);
        }
    }

    #[test]
    fn realloc_within_one_direct_mapping_keeps_the_pointer() {
        let allocator = Allocator::<TestMapper>::new();
        let old_size = (MAX_SMALL_SIZE + 1) as u64;
        let new_size = 5 * PAGE_SIZE as u64;
        let pointer = allocator.allocate(old_size, 8);
        assert!(!pointer.is_null());
        unsafe { pointer.write(0xa5) };

        // 16,385 and 20,480 bytes both use the same five-page mapping.
        let grown = unsafe { allocator.reallocate(pointer, old_size, new_size, 8) };
        assert_eq!(grown, pointer);
        assert_eq!(unsafe { grown.read() }, 0xa5);
        // SAFETY: the complete relabeled payload fits after the debug header,
        // including the last byte of the logical five-page capacity.
        unsafe { grown.add(new_size as usize - 1).write(0x5a) };

        // SAFETY: grown is live with the new layout.
        unsafe { allocator.deallocate(grown, new_size, 8) };
    }

    static COUNTED_MAPS: AtomicUsize = AtomicUsize::new(0);
    static COUNTED_UNMAPS: AtomicUsize = AtomicUsize::new(0);

    struct CountingMapper;

    // SAFETY: delegates mapping operations to TestMapper; atomic counters do
    // not weaken its validity or concurrency guarantees.
    unsafe impl PageMapper for CountingMapper {
        fn map(size: usize) -> *mut u8 {
            COUNTED_MAPS.fetch_add(1, Ordering::Relaxed);
            TestMapper::map(size)
        }

        unsafe fn unmap(pointer: *mut u8, size: usize) {
            COUNTED_UNMAPS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: inherited from PageMapper::unmap's contract.
            unsafe { TestMapper::unmap(pointer, size) };
        }
    }

    #[test]
    fn small_churn_reuses_one_arena_and_large_free_unmaps() {
        COUNTED_MAPS.store(0, Ordering::Relaxed);
        COUNTED_UNMAPS.store(0, Ordering::Relaxed);
        let allocator = Allocator::<CountingMapper>::new();

        for _ in 0..10_000 {
            let pointer = allocator.allocate(64, 8);
            assert!(!pointer.is_null());
            // SAFETY: pointer is live with this exact layout.
            unsafe { allocator.deallocate(pointer, 64, 8) };
        }
        assert_eq!(COUNTED_MAPS.load(Ordering::Relaxed), 1);
        assert_eq!(COUNTED_UNMAPS.load(Ordering::Relaxed), 0);

        let size = (MAX_SMALL_SIZE + 1) as u64;
        let direct = allocator.allocate(size, 8);
        assert!(!direct.is_null());
        // SAFETY: direct is live with this exact layout.
        unsafe { allocator.deallocate(direct, size, 8) };
        assert_eq!(COUNTED_MAPS.load(Ordering::Relaxed), 2);
        assert_eq!(COUNTED_UNMAPS.load(Ordering::Relaxed), 1);
    }

    #[cfg(debug_assertions)]
    struct UnmapTrapMapper;

    #[cfg(debug_assertions)]
    // SAFETY: mapping delegates to the test mapper; unmapping is deliberately
    // unreachable when layout validation runs before deallocation.
    unsafe impl PageMapper for UnmapTrapMapper {
        fn map(size: usize) -> *mut u8 {
            TestMapper::map(size)
        }

        unsafe fn unmap(_pointer: *mut u8, _size: usize) {
            panic!("unmap reached before layout validation");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "record.size == size")]
    fn wrong_direct_layout_panics_before_unmapping() {
        let allocator = Allocator::<UnmapTrapMapper>::new();
        let size = (MAX_SMALL_SIZE + 1) as u64;
        let pointer = allocator.allocate(size, 8);
        assert!(!pointer.is_null());
        unsafe { allocator.deallocate(pointer, size + 1, 8) };
    }

    static PERMITTED_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct PermittedCountingMapper;

    // SAFETY: delegates mapping operations to TestMapper and only counts the
    // allocation-permitted hook with an atomic counter.
    unsafe impl PageMapper for PermittedCountingMapper {
        fn allocation_permitted() -> bool {
            PERMITTED_CALLS.fetch_add(1, Ordering::Relaxed);
            true
        }

        fn map(size: usize) -> *mut u8 {
            TestMapper::map(size)
        }

        unsafe fn unmap(pointer: *mut u8, size: usize) {
            // SAFETY: inherited from PageMapper::unmap's contract.
            unsafe { TestMapper::unmap(pointer, size) };
        }
    }

    #[test]
    fn allocation_permitted_is_called_once_per_public_allocation_attempt() {
        PERMITTED_CALLS.store(0, Ordering::Relaxed);
        let allocator = Allocator::<PermittedCountingMapper>::new();
        let pointer = allocator.allocate(64, 8);
        assert!(!pointer.is_null());
        assert_eq!(PERMITTED_CALLS.load(Ordering::Relaxed), 1);
        unsafe { allocator.deallocate(pointer, 64, 8) };
    }

    static THREADED_ALLOCATOR: Allocator<TestMapper> = Allocator::new();

    #[test]
    fn concurrent_small_allocation_and_free_is_serialized() {
        let mut threads = std::vec::Vec::new();
        for thread_id in 0..8u8 {
            threads.push(std::thread::spawn(move || {
                for _ in 0..2_000 {
                    let pointer = THREADED_ALLOCATOR.allocate(64, 8);
                    assert!(!pointer.is_null());
                    unsafe {
                        pointer.write(thread_id);
                        std::thread::yield_now();
                        assert_eq!(pointer.read(), thread_id);
                        THREADED_ALLOCATOR.deallocate(pointer, 64, 8);
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

    static FAIL_MAPPING: AtomicBool = AtomicBool::new(false);

    struct FallibleMapper;

    // SAFETY: successful operations delegate to TestMapper, and the atomic
    // failure switch is safe to query concurrently.
    unsafe impl PageMapper for FallibleMapper {
        fn map(size: usize) -> *mut u8 {
            if FAIL_MAPPING.load(Ordering::Relaxed) {
                ptr::null_mut()
            } else {
                TestMapper::map(size)
            }
        }

        unsafe fn unmap(pointer: *mut u8, size: usize) {
            // SAFETY: inherited from PageMapper::unmap's contract.
            unsafe { TestMapper::unmap(pointer, size) };
        }
    }

    #[test]
    fn failed_realloc_preserves_direct_allocation() {
        let allocator = Allocator::<FallibleMapper>::new();
        let old_size = (MAX_SMALL_SIZE + 1) as u64;
        let pointer = allocator.allocate(old_size, 8);
        assert!(!pointer.is_null());
        unsafe { pointer.write(0xa5) };

        FAIL_MAPPING.store(true, Ordering::Relaxed);
        // SAFETY: pointer is live and readable for old_size bytes.
        let result = unsafe { allocator.reallocate(pointer, old_size, old_size * 2, 8) };
        FAIL_MAPPING.store(false, Ordering::Relaxed);

        assert!(result.is_null());
        assert_eq!(unsafe { pointer.read() }, 0xa5);
        // SAFETY: failed realloc left pointer live.
        unsafe { allocator.deallocate(pointer, old_size, 8) };
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn deallocate_rejects_a_wrong_same_class_layout() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(64, 8);
        assert!(!pointer.is_null());
        // 63 and 64 use the same logical class, but the caller's exact layout
        // contract still requires the original size.
        unsafe { allocator.deallocate(pointer, 63, 8) };
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn deallocate_rejects_a_wrong_direct_layout() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate((MAX_SMALL_SIZE + 1) as u64, 8);
        assert!(!pointer.is_null());
        // Both requests would round to the same mapping size.
        unsafe { allocator.deallocate(pointer, (MAX_SMALL_SIZE + 2) as u64, 8) };
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn deallocate_rejects_a_wrong_alignment_before_invalid_layout_classification() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(64, 8);
        assert!(!pointer.is_null());
        unsafe { allocator.deallocate(pointer, 64, 3) };
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn realloc_validates_before_copying() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(32, 8);
        assert!(!pointer.is_null());
        unsafe { allocator.reallocate(pointer, 31, 128, 8) };
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn resize_validates_an_invalid_old_layout_before_zero_size_return() {
        let allocator = Allocator::<TestMapper>::new();
        let pointer = allocator.allocate(32, 8);
        assert!(!pointer.is_null());
        unsafe { allocator.resize_in_place(pointer, 31, 0, 8) };
    }
}
