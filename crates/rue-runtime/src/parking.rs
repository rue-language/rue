//! Private pthread parking primitives for the hosted runtime.
//!
//! The pthread objects are deliberately kept opaque.  Their representation is
//! supplied by the target's libc (and differs between Linux and Darwin), so the
//! runtime must never reproduce a layout or size here.  Initialization pins the
//! storage and all operations retain that pin until the matching destroy call.

use core::cell::UnsafeCell;
use core::marker::{PhantomData, PhantomPinned};
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};

/// A pthread mutex whose storage cannot move after initialization.
pub(crate) struct ParkMutex {
    storage: UnsafeCell<MaybeUninit<libc::pthread_mutex_t>>,
    initialized: AtomicBool,
    _pin: PhantomPinned,
}

// The pthread implementation supplies the synchronization and the object is
// only accessed through its stable address.
unsafe impl Send for ParkMutex {}
unsafe impl Sync for ParkMutex {}

impl ParkMutex {
    pub(crate) const fn new() -> Self {
        Self {
            storage: UnsafeCell::new(MaybeUninit::uninit()),
            initialized: AtomicBool::new(false),
            _pin: PhantomPinned,
        }
    }

    pub(crate) fn init(self: Pin<&mut Self>) -> Result<(), i32> {
        let this = self.as_ref().get_ref();
        if this.initialized.load(Ordering::Acquire) {
            return Err(libc::EBUSY);
        }
        let mut attributes = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
        // ERRORCHECK makes accidental recursive locking a defined pthread
        // error instead of undefined behavior through the private safe API.
        let result = unsafe { libc::pthread_mutexattr_init(attributes.as_mut_ptr()) };
        if result != 0 {
            return Err(result);
        }
        let result = unsafe {
            libc::pthread_mutexattr_settype(attributes.as_mut_ptr(), libc::PTHREAD_MUTEX_ERRORCHECK)
        };
        if result != 0 {
            let _ = unsafe { libc::pthread_mutexattr_destroy(attributes.as_mut_ptr()) };
            return Err(result);
        }
        // SAFETY: the storage is uninitialized, exclusively pinned for this
        // call, and libc writes its target-defined object representation.
        let result = unsafe {
            libc::pthread_mutex_init((*this.storage.get()).as_mut_ptr(), attributes.as_ptr())
        };
        // The attributes object is independent of the initialized mutex.
        let _ = unsafe { libc::pthread_mutexattr_destroy(attributes.as_mut_ptr()) };
        if result == 0 {
            this.initialized.store(true, Ordering::Release);
            Ok(())
        } else {
            Err(result)
        }
    }

    fn raw(&self) -> *mut libc::pthread_mutex_t {
        self.storage.get().cast()
    }

    pub(crate) fn lock(self: Pin<&Self>) -> Result<ParkMutexGuard<'_>, i32> {
        if !self.get_ref().initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let result = unsafe { libc::pthread_mutex_lock(self.get_ref().raw()) };
        if result == 0 {
            Ok(ParkMutexGuard {
                mutex: self,
                _not_send: PhantomData,
            })
        } else {
            Err(result)
        }
    }

    /// Destroy the initialized mutex.
    ///
    /// # Safety
    ///
    /// The caller must prove that no guard is alive and no thread can access
    /// or wait on this mutex again. In particular, do not use `mem::forget` on
    /// a guard and then destroy the object while pthread still considers it
    /// locked. The pinned allocation must stay alive until every worker has
    /// returned from its final wait and has been joined. Worker cancellation
    /// or unwinding must not cross a pthread wait operation.
    pub(crate) unsafe fn destroy(self: Pin<&mut Self>) -> Result<(), i32> {
        // SAFETY: destroying consumes the pinned handle and never moves the
        // object; the raw pthread address remains stable until this returns.
        let this = unsafe { self.get_unchecked_mut() };
        if !this.initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let result = unsafe { libc::pthread_mutex_destroy(this.raw()) };
        if result == 0 {
            this.initialized.store(false, Ordering::Release);
            Ok(())
        } else {
            Err(result)
        }
    }
}

/// A same-thread mutex ownership token. It cannot cross a thread boundary and
/// unlocks exactly once when the protected scope ends.
pub(crate) struct ParkMutexGuard<'a> {
    mutex: Pin<&'a ParkMutex>,
    _not_send: PhantomData<*mut ()>,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{ParkCondvar, ParkMutex};
    use core::pin::Pin;
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::boxed::Box;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn lifecycle_errors_and_condition_binding_are_rejected() {
        let mut first = Box::pin(ParkMutex::new());
        assert!(matches!(first.as_ref().lock(), Err(libc::EINVAL)));
        first.as_mut().init().unwrap();
        assert_eq!(first.as_mut().init(), Err(libc::EBUSY));

        let mut second = Box::pin(ParkMutex::new());
        second.as_mut().init().unwrap();
        let mut condvar = Box::pin(ParkCondvar::new());
        condvar.as_mut().init().unwrap();

        let mut first_guard = first.as_ref().lock().unwrap();
        condvar
            .as_ref()
            .wait_while(&mut first_guard, || false)
            .unwrap();
        drop(first_guard);

        let mut second_guard = second.as_ref().lock().unwrap();
        assert_eq!(
            condvar.as_ref().wait_while(&mut second_guard, || false),
            Err(libc::EINVAL)
        );
        drop(second_guard);

        unsafe {
            condvar.as_mut().destroy().unwrap();
            second.as_mut().destroy().unwrap();
            first.as_mut().destroy().unwrap();
        }
    }

    #[test]
    fn mutex_condition_handoff_parks_and_wakes() {
        let mut mutex = Arc::new(ParkMutex::new());
        let mut condvar = Arc::new(ParkCondvar::new());
        // SAFETY: each object is freshly allocated and has not been moved
        // after this pin is created.
        unsafe { Pin::new_unchecked(Arc::get_mut(&mut mutex).unwrap()) }
            .init()
            .unwrap();
        unsafe { Pin::new_unchecked(Arc::get_mut(&mut condvar).unwrap()) }
            .init()
            .unwrap();

        let waiting = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let worker_mutex = Arc::clone(&mutex);
        let worker_condvar = Arc::clone(&condvar);
        let worker_waiting = Arc::clone(&waiting);
        let worker_ready = Arc::clone(&ready);
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let (watchdog_tx, watchdog_rx) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if watchdog_rx.recv_timeout(Duration::from_secs(10)).is_err() {
                // This covers a broken wait handoff that blocks the parent in
                // lock() or join(), where a channel timeout cannot help.
                std::process::abort();
            }
        });
        let worker = std::thread::spawn(move || {
            // SAFETY: the Arc keeps the object alive and it was initialized
            // before this reference was shared with the worker.
            let mutex = unsafe { Pin::new_unchecked(&*worker_mutex) };
            let condvar = unsafe { Pin::new_unchecked(&*worker_condvar) };
            let mut guard = mutex.lock().unwrap();
            condvar
                .wait_while(&mut guard, || {
                    worker_waiting.store(true, Ordering::Release);
                    let _ = waiting_tx.send(());
                    !worker_ready.load(Ordering::Acquire)
                })
                .unwrap();
            let result = worker_ready.load(Ordering::Acquire);
            let _ = done_tx.send(result);
            result
        });

        if waiting_rx.recv_timeout(Duration::from_secs(10)).is_err() {
            std::process::abort();
        }
        assert!(waiting.load(Ordering::Acquire));
        // SAFETY: this reference is pinned for the lifetime of the Arc and
        // the worker's guard ensures the pthread object remains initialized.
        let mutex_ref = unsafe { Pin::new_unchecked(&*mutex) };
        let guard = mutex_ref.lock().unwrap();
        ready.store(true, Ordering::Release);
        let condvar_ref = unsafe { Pin::new_unchecked(&*condvar) };
        condvar_ref.signal().unwrap();
        drop(guard);
        let worker_result = done_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| std::process::abort());
        assert!(worker_result);
        assert!(worker.join().unwrap());

        unsafe {
            Pin::new_unchecked(Arc::get_mut(&mut condvar).unwrap())
                .destroy()
                .unwrap();
            Pin::new_unchecked(Arc::get_mut(&mut mutex).unwrap())
                .destroy()
                .unwrap();
        }
        watchdog_tx.send(()).unwrap();
        watchdog.join().unwrap();
    }
}

impl ParkMutexGuard<'_> {
    fn raw(&self) -> *mut libc::pthread_mutex_t {
        self.mutex.get_ref().raw()
    }
}

impl Drop for ParkMutexGuard<'_> {
    fn drop(&mut self) {
        // The guard is created only after pthread_mutex_lock succeeds and is
        // !Send, so this unlock cannot be issued by a different owner. POSIX
        // therefore guarantees success; there is no recoverable error path at
        // a Rust destructor boundary.
        let result = unsafe { libc::pthread_mutex_unlock(self.raw()) };
        if result != 0 {
            // A guard can only be dropped by its locking thread. An unlock
            // failure therefore means the synchronization invariant is broken;
            // continuing could let later code observe protected state without
            // ownership.
            crate::platform::exit(101);
        }
    }
}

/// A pthread condition variable paired with a [`ParkMutex`].
pub(crate) struct ParkCondvar {
    storage: UnsafeCell<MaybeUninit<libc::pthread_cond_t>>,
    initialized: AtomicBool,
    bound_mutex: core::sync::atomic::AtomicUsize,
    _pin: PhantomPinned,
}

unsafe impl Send for ParkCondvar {}
unsafe impl Sync for ParkCondvar {}

impl ParkCondvar {
    pub(crate) const fn new() -> Self {
        Self {
            storage: UnsafeCell::new(MaybeUninit::uninit()),
            initialized: AtomicBool::new(false),
            bound_mutex: core::sync::atomic::AtomicUsize::new(0),
            _pin: PhantomPinned,
        }
    }

    pub(crate) fn init(self: Pin<&mut Self>) -> Result<(), i32> {
        let this = self.as_ref().get_ref();
        if this.initialized.load(Ordering::Acquire) {
            return Err(libc::EBUSY);
        }
        // SAFETY: the storage is uninitialized, exclusively pinned for this
        // call, and libc writes its target-defined object representation.
        let result = unsafe {
            libc::pthread_cond_init((*this.storage.get()).as_mut_ptr(), core::ptr::null())
        };
        if result == 0 {
            this.bound_mutex.store(0, Ordering::Release);
            this.initialized.store(true, Ordering::Release);
            Ok(())
        } else {
            Err(result)
        }
    }

    fn raw(&self) -> *mut libc::pthread_cond_t {
        self.storage.get().cast()
    }

    /// Park until `predicate` becomes false, rechecking it after every wake.
    ///
    /// POSIX permits spurious wakeups, so callers must express their state as a
    /// predicate and keep the mutex locked across the check and wait.
    pub(crate) fn wait_while(
        self: Pin<&Self>,
        mutex: &mut ParkMutexGuard<'_>,
        mut predicate: impl FnMut() -> bool,
    ) -> Result<(), i32> {
        if !self.get_ref().initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let mutex_address = mutex.mutex.get_ref() as *const ParkMutex as usize;
        match self.get_ref().bound_mutex.compare_exchange(
            0,
            mutex_address,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(bound) if bound == mutex_address => {}
            Err(_) => return Err(libc::EINVAL),
        }
        while predicate() {
            let result = unsafe { libc::pthread_cond_wait(self.get_ref().raw(), mutex.raw()) };
            if result != 0 {
                return Err(result);
            }
        }
        Ok(())
    }

    pub(crate) fn signal(self: Pin<&Self>) -> Result<(), i32> {
        if !self.get_ref().initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let result = unsafe { libc::pthread_cond_signal(self.get_ref().raw()) };
        if result == 0 { Ok(()) } else { Err(result) }
    }

    #[allow(dead_code)]
    pub(crate) fn broadcast(self: Pin<&Self>) -> Result<(), i32> {
        if !self.get_ref().initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let result = unsafe { libc::pthread_cond_broadcast(self.get_ref().raw()) };
        if result == 0 { Ok(()) } else { Err(result) }
    }

    /// Destroy the initialized condition variable.
    ///
    /// # Safety
    ///
    /// The caller must prove that no thread is waiting on the condition
    /// variable and that no future operation can access it. The pinned
    /// allocation must stay alive until every worker has returned from its
    /// final wait and has been joined. Worker cancellation or unwinding must
    /// not cross a pthread wait operation.
    pub(crate) unsafe fn destroy(self: Pin<&mut Self>) -> Result<(), i32> {
        // SAFETY: destroying consumes the pinned handle and never moves the
        // object; the raw pthread address remains stable until this returns.
        let this = unsafe { self.get_unchecked_mut() };
        if !this.initialized.load(Ordering::Acquire) {
            return Err(libc::EINVAL);
        }
        let result = unsafe { libc::pthread_cond_destroy(this.raw()) };
        if result == 0 {
            this.initialized.store(false, Ordering::Release);
            Ok(())
        } else {
            Err(result)
        }
    }
}
