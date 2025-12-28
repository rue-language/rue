use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub struct TracingAllocator {
    pub inner: System,
    pub stats: TracedStats,
}

impl TracingAllocator {
    #[allow(clippy::new_without_default)] // not applicable here
    pub const fn new() -> Self {
        Self {
            inner: System,
            stats: TracedStats::new(),
        }
    }

    pub fn stats(&self) -> &TracedStats {
        &self.stats
    }

    pub fn reset(&self) {
        self.stats.reset();
    }

    pub fn start_profiling(&self) {
        self.stats.start_profiling()
    }

    pub fn end_profiling(&self) {
        self.stats.end_profiling()
    }
}

unsafe impl GlobalAlloc for TracingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.stats.push_allocations(layout);
        self.inner.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.stats.push_deallocations(layout);
        self.inner.dealloc(ptr, layout);
    }
}

#[derive(Debug)]
pub struct TracedStats {
    is_active: AtomicBool,
    len_allocations: AtomicUsize,
    len_deallocations: AtomicUsize,
    current_memory_usage: AtomicUsize,
    total_memory_usage: AtomicUsize,
}

impl TracedStats {
    const fn new() -> Self {
        Self {
            is_active: AtomicBool::new(false),
            len_allocations: AtomicUsize::new(0),
            len_deallocations: AtomicUsize::new(0),
            current_memory_usage: AtomicUsize::new(0),
            total_memory_usage: AtomicUsize::new(0),
        }
    }

    pub fn len_allocations(&self) -> usize {
        self.len_allocations.load(Ordering::SeqCst)
    }

    pub fn len_deallocations(&self) -> usize {
        self.len_deallocations.load(Ordering::SeqCst)
    }

    pub fn current_allocated_bytes(&self) -> usize {
        self.current_memory_usage.load(Ordering::SeqCst)
    }

    pub fn total_allocated_bytes(&self) -> usize {
        self.total_memory_usage.load(Ordering::SeqCst)
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.len_allocations.store(0, Ordering::SeqCst);
        self.len_deallocations.store(0, Ordering::SeqCst);
        self.current_memory_usage.store(0, Ordering::SeqCst);
        self.total_memory_usage.store(0, Ordering::SeqCst);
    }

    fn start_profiling(&self) {
        self.is_active.store(true, Ordering::SeqCst);
    }

    fn end_profiling(&self) {
        self.is_active.store(false, Ordering::SeqCst);
    }

    fn push_allocations(&self, layout: Layout) {
        let size = layout.size();
        if !self.is_active() || size == 0 {
            return;
        }
        self.len_allocations.fetch_add(1, Ordering::SeqCst);
        self.current_memory_usage.fetch_add(size, Ordering::SeqCst);
        self.total_memory_usage.fetch_add(size, Ordering::SeqCst);
    }

    fn push_deallocations(&self, layout: Layout) {
        let size = layout.size();
        if !self.is_active() || size == 0 {
            return;
        }
        self.len_deallocations.fetch_add(1, Ordering::SeqCst);
        self.current_memory_usage.fetch_sub(size, Ordering::SeqCst);
    }
}
