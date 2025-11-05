//! Buffered I/O implementation for Rue runtime
//!
//! This module provides a buffered stdout implementation that can be called
//! from assembly code. It uses direct Linux syscalls for maximum performance and
//! minimal dependencies.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::abi::*;
use crate::syscall::{sys_exit, sys_write};

/// A wrapper that provides interior mutability for the buffer
struct BufferWrapper {
    buffer: UnsafeCell<[u8; STDOUT_BUFFER_SIZE]>,
    position: AtomicUsize,
}

// Safety: We ensure single-threaded access in Rue runtime
unsafe impl Sync for BufferWrapper {}

/// Global buffered stdout state
///
/// We use an UnsafeCell wrapper to provide interior mutability while avoiding
/// direct static mut access. The atomic position ensures we can track the buffer
/// position safely.
static STDOUT_STATE: BufferWrapper = BufferWrapper {
    buffer: UnsafeCell::new([0; STDOUT_BUFFER_SIZE]),
    position: AtomicUsize::new(0),
};

/// Buffered stdout implementation
///
/// This design uses UnsafeCell to centralize all unsafe access in one place,
/// providing better safety guarantees and easier auditing of unsafe code.
///
/// # Design Rationale
///
/// We use UnsafeCell here for several reasons:
/// 1. **Safety Centralization**: All unsafe buffer access happens through a single
///    controlled access point
/// 2. **Clippy Compliance**: Avoids direct static mut access which Clippy warns about
/// 3. **Interior Mutability**: Allows mutation through shared references, which is
///    needed for the C ABI functions
/// 4. **Clear Safety Boundaries**: Makes it explicit where unsafe operations occur
///
/// # Safety Invariants
///
/// - The buffer is only accessed through the with_buffer method
/// - Access is synchronized through the atomic position counter
/// - The implementation assumes single-threaded execution (as per Rue's design)
/// - Buffer bounds are always checked before access
pub struct BufferedStdout;

impl BufferedStdout {
    /// Perform an operation with the global buffer
    ///
    /// This approach encapsulates all unsafe access in one place
    #[inline]
    fn with_buffer<F, R>(f: F) -> R
    where
        F: FnOnce(&mut [u8], &AtomicUsize) -> R,
    {
        unsafe {
            let buffer = &mut *STDOUT_STATE.buffer.get();
            f(buffer, &STDOUT_STATE.position)
        }
    }

    /// Write a single byte to the buffer
    ///
    /// Auto-flushes if the buffer becomes full or if a newline is written.
    pub fn write_byte(byte: u8) -> Result<(), i32> {
        Self::with_buffer(|buffer, position| {
            let pos = position.load(Ordering::Relaxed);

            if pos >= STDOUT_BUFFER_SIZE {
                Self::flush_internal(buffer, position)?;
            }

            let current_pos = position.load(Ordering::Relaxed);
            buffer[current_pos] = byte;
            position.store(current_pos + 1, Ordering::Relaxed);

            if byte == b'\n' {
                Self::flush_internal(buffer, position)?;
            }

            Ok(())
        })
    }

    /// Write multiple bytes to the buffer
    ///
    /// For large writes that exceed the buffer size, this will write directly
    /// to stdout without buffering to avoid multiple flushes.
    pub fn write_bytes(data: &[u8]) -> Result<(), i32> {
        if data.is_empty() {
            return Ok(());
        }

        Self::with_buffer(|buffer, position| {
            let current_pos = position.load(Ordering::Relaxed);

            // If the data is larger than our buffer or would fill it completely,
            // flush current buffer and write directly
            if data.len() > STDOUT_BUFFER_SIZE || current_pos + data.len() >= STDOUT_BUFFER_SIZE {
                Self::flush_internal(buffer, position)?;
                return Self::write_direct(data);
            }

            // Write to buffer
            let new_pos = current_pos + data.len();
            buffer[current_pos..new_pos].copy_from_slice(data);
            position.store(new_pos, Ordering::Relaxed);

            // Check if we need to auto-flush on newline
            if data.contains(&b'\n') {
                Self::flush_internal(buffer, position)?;
            }

            Ok(())
        })
    }

    /// Flush the buffer to stdout
    pub fn flush() -> Result<(), i32> {
        Self::with_buffer(|buffer, position| Self::flush_internal(buffer, position))
    }

    /// Internal flush implementation that works with the buffer directly
    fn flush_internal(buffer: &[u8], position: &AtomicUsize) -> Result<(), i32> {
        let pos = position.load(Ordering::Relaxed);
        if pos == 0 {
            return Ok(()); // Nothing to flush
        }

        let result = Self::write_direct(&buffer[..pos]);
        position.store(0, Ordering::Relaxed);
        result
    }

    /// Write data directly to stdout using syscall
    fn write_direct(data: &[u8]) -> Result<(), i32> {
        if data.is_empty() {
            return Ok(());
        }

        let result = unsafe { sys_write(STDOUT_FD, data.as_ptr(), data.len()) };

        match result {
            Ok(_) => Ok(()),
            Err(_) => {
                // Exit on syscall failure
                unsafe {
                    sys_exit(EXIT_SYSCALL_FAILED);
                }
            }
        }
    }
}

/// C ABI function: Write a single byte to buffered stdout
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_write_byte(byte: u8) {
    let _ = BufferedStdout::write_byte(byte);
}

/// C ABI function: Write multiple bytes to buffered stdout
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions. The caller must ensure that `ptr` points to valid
/// memory of at least `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_write_bytes(ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }

    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    let _ = BufferedStdout::write_bytes(data);
}

/// C ABI function: Flush the stdout buffer
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_flush_stdout() {
    let _ = BufferedStdout::flush();
}
