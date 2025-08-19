//! Buffered I/O implementation for Rue runtime
//!
//! This module provides a buffered stdout implementation that can be called
//! from assembly code. It uses direct Linux syscalls for maximum performance and
//! minimal dependencies.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Buffer size for stdout buffering (4KB)
const STDOUT_BUFFER_SIZE: usize = 4096;

/// Linux syscall numbers
const SYS_WRITE: i64 = 1;
const SYS_EXIT: i64 = 60;

/// File descriptors
const STDOUT_FD: i32 = 1;

/// Exit codes
const EXIT_CODE_SYSCALL_FAILED: i32 = 253;

/// Global buffered stdout instance
static mut STDOUT_BUFFER: [u8; STDOUT_BUFFER_SIZE] = [0; STDOUT_BUFFER_SIZE];
static STDOUT_POS: AtomicUsize = AtomicUsize::new(0);

/// Buffered stdout implementation
///
/// Uses raw pointers to avoid issues with multiple mutable borrows
/// and to comply with Clippy's recommendations for accessing static mutable data.
pub struct BufferedStdout {
    buffer: *mut [u8; STDOUT_BUFFER_SIZE],
    position: &'static AtomicUsize,
}

impl BufferedStdout {
    /// Get a reference to the global buffered stdout instance
    ///
    /// # Safety
    /// This function is unsafe because it provides mutable access to global state.
    /// The caller must ensure this is only called from a single thread or properly
    /// synchronized.
    unsafe fn get_instance() -> Self {
        Self {
            buffer: &raw mut STDOUT_BUFFER,
            position: &STDOUT_POS,
        }
    }

    /// Write a single byte to the buffer
    ///
    /// Auto-flushes if the buffer becomes full or if a newline is written.
    fn write_byte(&mut self, byte: u8) -> Result<(), i32> {
        let pos = self.position.load(Ordering::Relaxed);

        // Check if buffer is full
        if pos >= STDOUT_BUFFER_SIZE {
            self.flush()?;
        }

        // Write the byte
        let current_pos = self.position.load(Ordering::Relaxed);
        unsafe {
            (*self.buffer)[current_pos] = byte;
        }
        self.position.store(current_pos + 1, Ordering::Relaxed);

        // Auto-flush on newline
        if byte == b'\n' {
            self.flush()?;
        }

        Ok(())
    }

    /// Write multiple bytes to the buffer
    ///
    /// For large writes that exceed the buffer size, this will write directly
    /// to stdout without buffering to avoid multiple flushes.
    fn write_bytes(&mut self, data: &[u8]) -> Result<(), i32> {
        let current_pos = self.position.load(Ordering::Relaxed);

        // If the data is larger than our buffer or would fill it completely,
        // flush current buffer and write directly
        if data.len() > STDOUT_BUFFER_SIZE || current_pos + data.len() >= STDOUT_BUFFER_SIZE {
            self.flush()?;
            return self.write_direct(data);
        }

        // Write to buffer
        let new_pos = current_pos + data.len();
        unsafe {
            let buffer_slice = &mut (&mut (*self.buffer))[current_pos..new_pos];
            buffer_slice.copy_from_slice(data);
        }
        self.position.store(new_pos, Ordering::Relaxed);

        // Check if we need to auto-flush on newline
        if data.contains(&b'\n') {
            self.flush()?;
        }

        Ok(())
    }

    /// Flush the buffer to stdout
    fn flush(&mut self) -> Result<(), i32> {
        let pos = self.position.load(Ordering::Relaxed);
        if pos == 0 {
            return Ok(()); // Nothing to flush
        }

        let result = unsafe {
            let buffer_slice = &(&(*self.buffer))[..pos];
            self.write_direct(buffer_slice)
        };
        self.position.store(0, Ordering::Relaxed);
        result
    }

    /// Write data directly to stdout using syscall
    fn write_direct(&self, data: &[u8]) -> Result<(), i32> {
        if data.is_empty() {
            return Ok(());
        }

        let result = unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FD as i64,
                data.as_ptr() as i64,
                data.len() as i64,
            )
        };

        if result < 0 {
            // Exit on syscall failure
            unsafe {
                syscall1(SYS_EXIT, EXIT_CODE_SYSCALL_FAILED as i64);
            }
            // This should never be reached, but return error for completeness
            Err(EXIT_CODE_SYSCALL_FAILED)
        } else {
            Ok(())
        }
    }
}

/// Raw syscall wrapper for 1 argument
#[inline(always)]
unsafe fn syscall1(syscall_num: i64, arg1: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Raw syscall wrapper for 3 arguments
#[inline(always)]
unsafe fn syscall3(syscall_num: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// C ABI function: Write a single byte to buffered stdout
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_write_byte(byte: u8) {
    unsafe {
        let mut stdout = BufferedStdout::get_instance();
        let _ = stdout.write_byte(byte);
    }
}

/// C ABI function: Write multiple bytes to buffered stdout
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions. The caller must ensure that `ptr` points to valid
/// memory of at least `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_write_bytes(ptr: *const u8, len: usize) {
    unsafe {
        if ptr.is_null() || len == 0 {
            return;
        }

        let data = core::slice::from_raw_parts(ptr, len);
        let mut stdout = BufferedStdout::get_instance();
        let _ = stdout.write_bytes(data);
    }
}

/// C ABI function: Flush the stdout buffer
///
/// # Safety
/// This function is intended to be called from assembly code and must maintain
/// C calling conventions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_flush_stdout() {
    unsafe {
        let mut stdout = BufferedStdout::get_instance();
        let _ = stdout.flush();
    }
}
