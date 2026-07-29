//! Control Flow Graph IR for the Rue compiler.
//!
//! This crate provides a CFG-based intermediate representation that sits
//! between AIR (typed, structured) and X86Mir (machine-specific).
//!
//! The CFG representation makes control flow explicit through basic blocks
//! and terminators, which is essential for:
//! - Linear type checking
//! - Drop elaboration
//! - Liveness analysis
//! - Other dataflow analyses
//!
//! ## Pipeline
//!
//! ```text
//! AIR (structured) → CFG (explicit control flow) → X86Mir (machine code)
//! ```

#[cfg(test)]
mod api_inventory;
mod build;
mod dominators;
mod inline;
mod inst;
pub mod opt;
mod payload;
mod verify;

use rue_error::{CompileError, CompileWarning};

pub use build::CfgBuilder;
pub use inline::{CfgInlineError, inline_call};
pub use inst::{
    BasicBlock, BlockId, Cfg, CfgArgMode, CfgCallArg, CfgDisplay, CfgEditError,
    CfgEditTransactionError, CfgEditor, CfgInst, CfgInstData, CfgPayloadStorageStats,
    CfgRemapError, CfgValue, Place, PlaceBase, Projection, Terminator, ValidatedCfg,
};
pub use opt::OptLevel;
#[doc(hidden)]
#[cfg(any(test, feature = "fuzz-support"))]
pub use payload::fuzz_payload_corruption;
pub use payload::{CFG_PAYLOAD_FAMILY_NAMES, PayloadError};
pub use verify::{CfgVerificationError, CfgVerificationLocation};

// Re-export types from rue-air that we use
pub use rue_air::{StructDef, StructId, Type, TypeKind};

/// Output from CFG construction.
///
/// Contains the constructed CFG along with any warnings detected during
/// construction (e.g., unreachable code).
pub struct CfgOutput {
    /// The constructed control flow graph.
    pub cfg: Option<ValidatedCfg>,
    /// Warnings detected during CFG construction.
    pub warnings: Vec<CompileWarning>,
    /// Internal-compiler-error diagnostics detected during CFG construction
    /// (RUE-7). Non-empty only for malformed AIR that upstream passes should
    /// have ruled out (e.g. an un-specialized `CallGeneric`). The driver must
    /// treat a non-empty `errors` as a hard failure and abort before
    /// optimizing or lowering the (now-discarded) CFG.
    pub errors: Vec<CompileError>,
    /// Named user destructors required by implicit drops emitted in this CFG.
    pub implicit_named_destructors: Vec<StructId>,
    /// Whether an implicit destructor target was anonymous and therefore has
    /// no stable definition endpoint.
    pub anonymous_destructor_dependency_incomplete: bool,
}

#[cfg(test)]
pub(crate) mod allocation_test_support {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ACTIVE: Cell<bool> = const { Cell::new(false) };
        static COUNT: Cell<usize> = const { Cell::new(0) };
        static BYTES: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ACTIVE.with(|active| {
                if active.get() {
                    COUNT.with(|count| count.set(count.get() + 1));
                    BYTES.with(|bytes| bytes.set(bytes.get() + layout.size()));
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    pub(crate) fn allocations_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
        let (result, count, _) = allocation_evidence(f);
        (result, count)
    }

    pub(crate) fn allocation_evidence<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
        COUNT.with(|count| count.set(0));
        BYTES.with(|bytes| bytes.set(0));
        ACTIVE.with(|active| active.set(true));
        let result = f();
        ACTIVE.with(|active| active.set(false));
        let count = COUNT.with(Cell::get);
        let bytes = BYTES.with(Cell::get);
        (result, count, bytes)
    }
}
