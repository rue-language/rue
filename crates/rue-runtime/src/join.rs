//! Private hosted implementation of the scoped `join_inout` operation.
//!
//! After `pthread_create`, the pinned frame is shared through immutable
//! references. Only the protocol in `UnsafeCell` is mutable, and each access
//! is made while the parked mutex is held. This keeps the Rust aliasing proof
//! separate from the pthread synchronization proof.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::pin::Pin;

use crate::fault::{self, AltSignalStack, WorkerSlotToken};
use crate::parking::{ParkCondvar, ParkMutex};

#[cfg(test)]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SUCCESS: u32 = 0;
const RESOURCE_EXHAUSTED: u32 = 1;
const PRESTART_FAILURE: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum JoinState {
    Initializing,
    Ready,
    Failed(u32),
    Start,
    Finished,
}

struct ProtocolState {
    state: JoinState,
    token: Option<WorkerSlotToken>,
}

/// The private descriptor shared by the source-owned task adapters that will
/// be added later. Context and executable code remain distinct operands.
#[derive(Clone, Copy)]
struct TaskDescriptor {
    entry: unsafe extern "C" fn(*mut u8, *mut u8),
    context: *mut u8,
    code: *mut u8,
}

type NativeCallback = unsafe extern "C" fn(*mut u8);

#[cfg(test)]
static TEST_MUTEX_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_MUTEX_DESTROYS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_CONDVAR_INITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_CONDVAR_DESTROYS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_PTHREAD_CREATES: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_PTHREAD_JOINS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn test_mutex_inits() -> usize {
    TEST_MUTEX_INITS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn test_mutex_destroys() -> usize {
    TEST_MUTEX_DESTROYS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn test_condvar_inits() -> usize {
    TEST_CONDVAR_INITS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn test_condvar_destroys() -> usize {
    TEST_CONDVAR_DESTROYS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn test_pthread_creates() -> usize {
    TEST_PTHREAD_CREATES.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn test_pthread_joins() -> usize {
    TEST_PTHREAD_JOINS.load(Ordering::Acquire)
}

struct JoinFrame {
    mutex: ParkMutex,
    condvar: ParkCondvar,
    protocol: UnsafeCell<ProtocolState>,
    stack: Option<AltSignalStack>,
    worker: TaskDescriptor,
    parent: TaskDescriptor,
    #[cfg(test)]
    child_live: AtomicBool,
}

// The mutex protects every protocol access. The stack and descriptors are
// immutable after the frame is pinned, and the parent destroys them only after
// pthread_join. The two context pointers are disjoint exclusive loans owned by
// the caller for the complete operation; each callback only receives its own
// pointer. The code pointers are the exact audited native callback shape below.
// These facts, plus the no-unwind callback contract, justify sharing this raw
// pointer-bearing frame through an immutable reference.
unsafe impl Sync for JoinFrame {}

unsafe extern "C" fn callback_entry(context: *mut u8, code: *mut u8) {
    if context.is_null() || code.is_null() {
        fatal();
    }
    // SAFETY: typed lowering supplies an ordinary native
    // `extern "C" fn(*mut u8)` in the executable-code slot. This is the only
    // opaque code-pointer conversion in the runtime.
    let callback: NativeCallback = unsafe { core::mem::transmute(code) };
    unsafe { callback(context) };
}

impl JoinFrame {
    fn new(
        token: WorkerSlotToken,
        stack: AltSignalStack,
        worker: TaskDescriptor,
        parent: TaskDescriptor,
    ) -> Self {
        Self {
            mutex: ParkMutex::new(),
            condvar: ParkCondvar::new(),
            protocol: UnsafeCell::new(ProtocolState {
                state: JoinState::Initializing,
                token: Some(token),
            }),
            stack: Some(stack),
            worker,
            parent,
            #[cfg(test)]
            child_live: AtomicBool::new(false),
        }
    }

    fn mutex(&self) -> Pin<&ParkMutex> {
        // SAFETY: the frame is pinned for the complete pthread lifetime.
        unsafe { Pin::new_unchecked(&self.mutex) }
    }

    fn condvar(&self) -> Pin<&ParkCondvar> {
        // SAFETY: the frame is pinned for the complete pthread lifetime.
        unsafe { Pin::new_unchecked(&self.condvar) }
    }

    unsafe fn state(&self) -> JoinState {
        // SAFETY: the caller holds mutex for this short-lived read.
        unsafe { (*self.protocol.get()).state }
    }

    unsafe fn set_state(&self, state: JoinState) {
        // SAFETY: the caller holds mutex for this write.
        unsafe { (*self.protocol.get()).state = state };
    }

    unsafe fn take_token(&self) -> Option<WorkerSlotToken> {
        // SAFETY: the child takes this once while holding mutex, before
        // publishing readiness; no other thread accesses it afterward.
        unsafe { (*self.protocol.get()).token.take() }
    }
}

fn set_child_ready(frame: &JoinFrame, result: Result<(), u32>) {
    let guard = frame.mutex().lock().unwrap_or_else(|_| fatal());
    unsafe {
        frame.set_state(if result.is_ok() {
            JoinState::Ready
        } else {
            JoinState::Failed(result.err().unwrap_or(PRESTART_FAILURE))
        });
    }
    frame.condvar().signal().unwrap_or_else(|_| fatal());
    drop(guard);
}

fn wait_for_parent_start(frame: &JoinFrame) {
    let mut guard = frame.mutex().lock().unwrap_or_else(|_| fatal());
    frame
        .condvar()
        .wait_while(&mut guard, || unsafe { frame.state() == JoinState::Ready })
        .unwrap_or_else(|_| fatal());
    if unsafe { frame.state() } != JoinState::Start {
        fatal();
    }
    drop(guard);
}

fn mark_finished(frame: &JoinFrame) {
    let guard = frame.mutex().lock().unwrap_or_else(|_| fatal());
    unsafe { frame.set_state(JoinState::Finished) };
    frame.condvar().signal().unwrap_or_else(|_| fatal());
    drop(guard);
}

fn invoke_descriptor(descriptor: TaskDescriptor) {
    // SAFETY: the entry is the fixed callback bridge above.
    unsafe { (descriptor.entry)(descriptor.context, descriptor.code) };
}

fn resource_status(error: i32) -> u32 {
    if error == libc::EAGAIN || error == libc::ENOMEM {
        RESOURCE_EXHAUSTED
    } else {
        PRESTART_FAILURE
    }
}

extern "C" fn worker_trampoline(argument: *mut c_void) -> *mut c_void {
    // SAFETY: pthread_create receives a pinned frame and the parent joins
    // before it is reclaimed.
    let frame = unsafe { &*(argument.cast::<JoinFrame>()) };
    let token = {
        let guard = frame.mutex().lock().unwrap_or_else(|_| fatal());
        let token = unsafe { frame.take_token() };
        drop(guard);
        token.unwrap_or_else(|| fatal())
    };

    // SAFETY: this mapping is dedicated to this pthread and retained through
    // join, including the initialization-failure path.
    let stack = frame.stack.as_ref().unwrap_or_else(|| fatal());
    let registration = match initialize_worker(token, stack) {
        Ok(registration) => {
            set_child_ready(frame, Ok(()));
            Some(registration)
        }
        Err(error) => {
            set_child_ready(frame, Err(resource_status(error)));
            None
        }
    };
    let Some(registration) = registration else {
        return core::ptr::null_mut();
    };

    wait_for_parent_start(frame);
    invoke_descriptor(frame.worker);
    drop(registration);
    mark_finished(frame);
    core::ptr::null_mut()
}

fn initialize_worker(
    token: WorkerSlotToken,
    stack: &AltSignalStack,
) -> Result<fault::WorkerRegistration, i32> {
    #[cfg(test)]
    if fault::take_test_failure(fault::TestFailurePoint::WorkerRegistration) {
        // Fail before the platform registration call to exercise the first
        // worker-initialization acquisition boundary independently.
        return Err(libc::EPERM);
    }
    #[cfg(test)]
    if fault::take_test_failure(fault::TestFailurePoint::WorkerInitialization) {
        // SAFETY: this test seam performs the same real alt-stack installation
        // as production, then injects only the bounds-query failure. The
        // parent keeps the mapping live until this pthread is joined.
        return unsafe {
            fault::initialize_worker_with_bounds_for_test(token, stack, Err(libc::ERANGE))
        };
    }
    // SAFETY: run_tasks dedicates this mapping to this pthread and retains it
    // until the pthread has returned and the parent has joined it.
    unsafe { fault::initialize_worker(token, stack) }
}

fn mutex_init(frame: &mut JoinFrame) -> Result<(), i32> {
    #[cfg(test)]
    if fault::take_test_failure(fault::TestFailurePoint::MutexInit) {
        return Err(libc::ENOMEM);
    }
    // SAFETY: the frame is not published until initialization completes.
    let result = unsafe { Pin::new_unchecked(&mut frame.mutex) }.init();
    #[cfg(test)]
    if result.is_ok() {
        TEST_MUTEX_INITS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

unsafe fn mutex_destroy(frame: &mut JoinFrame) -> Result<(), i32> {
    // SAFETY: caller proves no guard or waiter remains.
    let result = unsafe { Pin::new_unchecked(&mut frame.mutex).destroy() };
    #[cfg(test)]
    if result.is_ok() {
        TEST_MUTEX_DESTROYS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

fn condvar_init(frame: &mut JoinFrame) -> Result<(), i32> {
    #[cfg(test)]
    if fault::take_test_failure(fault::TestFailurePoint::CondvarInit) {
        return Err(libc::ENOMEM);
    }
    // SAFETY: the frame is not published until initialization completes.
    let result = unsafe { Pin::new_unchecked(&mut frame.condvar) }.init();
    #[cfg(test)]
    if result.is_ok() {
        TEST_CONDVAR_INITS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

unsafe fn condvar_destroy(frame: &mut JoinFrame) -> Result<(), i32> {
    // SAFETY: caller proves no waiter remains.
    let result = unsafe { Pin::new_unchecked(&mut frame.condvar).destroy() };
    #[cfg(test)]
    if result.is_ok() {
        TEST_CONDVAR_DESTROYS.fetch_add(1, Ordering::AcqRel);
    }
    result
}

unsafe fn cleanup_quiescent(frame: &mut JoinFrame, condvar: bool, mutex: bool) {
    #[cfg(test)]
    assert!(
        !frame.child_live.load(Ordering::Acquire),
        "operation resources reclaimed before its child was joined"
    );
    if condvar && unsafe { condvar_destroy(frame) }.is_err() {
        fatal();
    }
    if mutex && unsafe { mutex_destroy(frame) }.is_err() {
        fatal();
    }
    // SAFETY: no live child remains, so no signal can execute on this mapping.
    if let Some(stack) = frame.stack.take()
        && unsafe { fault::destroy_worker_alt_stack(stack) }.is_err()
    {
        fatal();
    }
}

unsafe fn create_worker(child: *mut libc::pthread_t, frame: *const JoinFrame) -> i32 {
    #[cfg(test)]
    if fault::take_test_failure(fault::TestFailurePoint::Create) {
        return libc::EAGAIN;
    }
    // SAFETY: run_tasks provides parent-local uninitialized pthread storage
    // and retains the pinned frame until the created child is joined.
    unsafe {
        libc::pthread_create(
            child,
            core::ptr::null(),
            worker_trampoline,
            frame.cast_mut().cast::<c_void>(),
        )
    }
}

/// Run two typed callbacks concurrently and join both before returning.
/// Nonzero status means no user callback has begun and both contexts remain
/// owned by the caller.
///
/// # Safety
///
/// The caller must keep both distinct context allocations and their exact
/// `extern "C" fn(*mut u8) -> ()` code pointers valid and exclusively loaned
/// until this function returns. Neither callback may unwind across this FFI
/// boundary. The child alone installs the dedicated alternate stack; its
/// mapping and this pinned frame remain live until the parent has joined it.
unsafe fn run_tasks(worker: TaskDescriptor, parent: TaskDescriptor) -> u32 {
    let reservation = match fault::reserve_worker_slot() {
        Ok(reservation) => reservation,
        Err(error) => return resource_status(error),
    };
    let stack = match fault::allocate_worker_alt_stack() {
        Ok(stack) => stack,
        Err(error) => return resource_status(error),
    };
    let token = reservation.handoff();
    let mut frame = core::pin::pin!(JoinFrame::new(token, stack, worker, parent));
    let mut child = MaybeUninit::<libc::pthread_t>::uninit();

    let mutex_initialized = match mutex_init(unsafe { Pin::get_unchecked_mut(frame.as_mut()) }) {
        Ok(()) => true,
        Err(error) => {
            let frame_mut = unsafe { Pin::get_unchecked_mut(frame.as_mut()) };
            // SAFETY: no child exists and this mapping is still parent-owned.
            if let Some(stack) = frame_mut.stack.take()
                && unsafe { fault::destroy_worker_alt_stack(stack) }.is_err()
            {
                fatal();
            }
            return resource_status(error);
        }
    };
    let condvar_initialized = match condvar_init(unsafe { Pin::get_unchecked_mut(frame.as_mut()) })
    {
        Ok(()) => true,
        Err(error) => {
            let frame_mut = unsafe { Pin::get_unchecked_mut(frame.as_mut()) };
            // SAFETY: no child exists and the mutex is unlocked.
            unsafe { cleanup_quiescent(frame_mut, false, mutex_initialized) };
            return resource_status(error);
        }
    };

    let frame_ptr = frame.as_ref().get_ref() as *const JoinFrame;
    let create_result = unsafe { create_worker(child.as_mut_ptr(), frame_ptr) };
    if create_result != 0 {
        let frame_mut = unsafe { Pin::get_unchecked_mut(frame.as_mut()) };
        // SAFETY: pthread_create failed, so no child can access the frame.
        unsafe { cleanup_quiescent(frame_mut, condvar_initialized, mutex_initialized) };
        return resource_status(create_result);
    }
    #[cfg(test)]
    TEST_PTHREAD_CREATES.fetch_add(1, Ordering::AcqRel);
    #[cfg(test)]
    frame.child_live.store(true, Ordering::Release);

    let frame_ref = unsafe { &*frame_ptr };
    let mut guard = frame_ref.mutex().lock().unwrap_or_else(|_| fatal());
    frame_ref
        .condvar()
        .wait_while(&mut guard, || unsafe {
            frame_ref.state() == JoinState::Initializing
        })
        .unwrap_or_else(|_| fatal());
    let status = match unsafe { frame_ref.state() } {
        JoinState::Failed(status) => {
            drop(guard);
            status
        }
        JoinState::Ready => {
            unsafe { frame_ref.set_state(JoinState::Start) };
            frame_ref.condvar().signal().unwrap_or_else(|_| fatal());
            drop(guard);

            invoke_descriptor(frame_ref.parent);

            let mut guard = frame_ref.mutex().lock().unwrap_or_else(|_| fatal());
            frame_ref
                .condvar()
                .wait_while(&mut guard, || unsafe {
                    frame_ref.state() != JoinState::Finished
                })
                .unwrap_or_else(|_| fatal());
            drop(guard);
            SUCCESS
        }
        _ => fatal(),
    };
    // A failed initialization can still have installed an alternate stack.
    // Both outcomes require the same real join before any resource teardown.
    let join_result = unsafe { libc::pthread_join(child.as_ptr().read(), core::ptr::null_mut()) };
    if join_result != 0 {
        fatal();
    }
    #[cfg(test)]
    {
        TEST_PTHREAD_JOINS.fetch_add(1, Ordering::AcqRel);
        frame_ref.child_live.store(false, Ordering::Release);
    }
    let frame_mut = unsafe { Pin::get_unchecked_mut(frame.as_mut()) };
    // SAFETY: the child is joined and all guards have been dropped.
    unsafe { cleanup_quiescent(frame_mut, condvar_initialized, mutex_initialized) };
    status
}

/// Hosted ABI wrapper. The compiler supplies ordinary native callback code
/// pointers and the contexts they exclusively own for the scoped call.
pub(crate) unsafe fn __rue_join_inout(
    left_context: *mut u8,
    left_code: *mut u8,
    right_context: *mut u8,
    right_code: *mut u8,
) -> u32 {
    if left_context.is_null()
        || left_code.is_null()
        || right_context.is_null()
        || right_code.is_null()
    {
        return PRESTART_FAILURE;
    }
    let worker = TaskDescriptor {
        entry: callback_entry,
        context: left_context,
        code: left_code,
    };
    let parent = TaskDescriptor {
        entry: callback_entry,
        context: right_context,
        code: right_code,
    };
    // SAFETY: the compiler's scoped lowering proves context ownership and the
    // ordinary callback ABI before reaching this wrapper.
    unsafe { run_tasks(worker, parent) }
}

fn fatal() -> ! {
    crate::platform::exit(101)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{TaskDescriptor, callback_entry, run_tasks};
    use crate::fault::{self, TestFailurePoint};
    use core::sync::atomic::{AtomicUsize, Ordering};

    struct Probe {
        calls: AtomicUsize,
        value: AtomicUsize,
    }

    unsafe extern "C" fn worker_callback(context: *mut u8) {
        // SAFETY: every test descriptor points at a live Probe until run_tasks
        // has joined the worker and returned.
        let probe = unsafe { &*(context.cast::<Probe>()) };
        probe.calls.fetch_add(1, Ordering::Release);
        probe.value.store(101, Ordering::Release);
    }

    unsafe extern "C" fn parent_callback(context: *mut u8) {
        // SAFETY: the parent descriptor points at a live Probe for this call.
        let probe = unsafe { &*(context.cast::<Probe>()) };
        probe.calls.fetch_add(1, Ordering::Release);
        probe.value.store(202, Ordering::Release);
    }

    fn code(callback: unsafe extern "C" fn(*mut u8)) -> *mut u8 {
        callback as *const () as *mut u8
    }

    fn descriptors(worker: &Probe, parent: &Probe) -> (TaskDescriptor, TaskDescriptor) {
        (
            TaskDescriptor {
                entry: callback_entry,
                context: (worker as *const Probe).cast_mut().cast(),
                code: code(worker_callback),
            },
            TaskDescriptor {
                entry: callback_entry,
                context: (parent as *const Probe).cast_mut().cast(),
                code: code(parent_callback),
            },
        )
    }

    fn assert_registry_capacity_is_restored() {
        let mut reservations = std::vec::Vec::new();
        for _ in 0..(fault::WORKER_SLOT_COUNT - 1) {
            reservations.push(fault::reserve_worker_slot().expect("reserve recovered slot"));
        }
        assert!(matches!(
            fault::reserve_worker_slot(),
            Err(error) if error == libc::EAGAIN
        ));
        drop(reservations);
    }

    fn assert_failure_resource_balance(
        before: fault::TestResourceCounts,
        after: fault::TestResourceCounts,
        point: TestFailurePoint,
    ) {
        assert_eq!(
            after.slot_acquires - before.slot_acquires,
            after.slot_releases - before.slot_releases,
            "slot reservation leaked after {point:?}"
        );
        assert_eq!(
            after.stack_allocations - before.stack_allocations,
            after.stack_destroys - before.stack_destroys,
            "alternate stack mapping leaked after {point:?}"
        );
        assert_eq!(
            after.mutex_inits - before.mutex_inits,
            after.mutex_destroys - before.mutex_destroys,
            "mutex leaked after {point:?}"
        );
        assert_eq!(
            after.condvar_inits - before.condvar_inits,
            after.condvar_destroys - before.condvar_destroys,
            "condition variable leaked after {point:?}"
        );
        assert_eq!(
            after.pthread_creates - before.pthread_creates,
            after.pthread_joins - before.pthread_joins,
            "child pthread leaked after {point:?}"
        );
        let expected_registrations = usize::from(point == TestFailurePoint::WorkerInitialization);
        assert_eq!(
            after.stack_registrations - before.stack_registrations,
            expected_registrations,
            "unexpected alternate-stack registration count after {point:?}"
        );
    }

    unsafe extern "C" fn nested_callback(context: *mut u8) {
        let worker = Probe {
            calls: AtomicUsize::new(0),
            value: AtomicUsize::new(0),
        };
        let parent = Probe {
            calls: AtomicUsize::new(0),
            value: AtomicUsize::new(0),
        };
        let (worker_descriptor, parent_descriptor) = descriptors(&worker, &parent);
        // SAFETY: the nested probes are distinct and remain live through join.
        let status = unsafe { run_tasks(worker_descriptor, parent_descriptor) };
        let outer = unsafe { &*context.cast::<Probe>() };
        outer.calls.fetch_add(1, Ordering::Release);
        outer.value.store(
            if status == 0 {
                worker.value.load(Ordering::Acquire) + parent.value.load(Ordering::Acquire)
            } else {
                0
            },
            Ordering::Release,
        );
    }

    #[test]
    fn nested_callbacks_reclaim_only_their_own_child_resources() {
        let _serial = fault::REGISTRY_TEST_LOCK.lock().unwrap();
        let worker = Probe {
            calls: AtomicUsize::new(0),
            value: AtomicUsize::new(0),
        };
        let parent = Probe {
            calls: AtomicUsize::new(0),
            value: AtomicUsize::new(0),
        };
        let (mut worker_descriptor, mut parent_descriptor) = descriptors(&worker, &parent);
        worker_descriptor.code = code(nested_callback);
        parent_descriptor.code = code(nested_callback);
        let before = fault::test_resource_counts();
        // SAFETY: distinct outer probes remain live until all three joins finish.
        assert_eq!(
            unsafe { run_tasks(worker_descriptor, parent_descriptor) },
            0
        );
        let after = fault::test_resource_counts();
        assert_eq!(worker.calls.load(Ordering::Acquire), 1);
        assert_eq!(parent.calls.load(Ordering::Acquire), 1);
        assert_eq!(worker.value.load(Ordering::Acquire), 303);
        assert_eq!(parent.value.load(Ordering::Acquire), 303);
        assert_eq!(after.pthread_creates - before.pthread_creates, 3);
        assert_eq!(after.pthread_joins - before.pthread_joins, 3);
        assert_eq!(after.stack_allocations - before.stack_allocations, 3);
        assert_eq!(after.stack_destroys - before.stack_destroys, 3);
        assert_registry_capacity_is_restored();
    }

    #[test]
    fn every_prestart_acquisition_failure_preserves_contexts_and_releases_resources() {
        let _serial = fault::REGISTRY_TEST_LOCK.lock().unwrap();
        for (point, expected) in [
            (TestFailurePoint::Reserve, 1),
            (TestFailurePoint::Allocate, 1),
            (TestFailurePoint::MutexInit, 1),
            (TestFailurePoint::CondvarInit, 1),
            (TestFailurePoint::Create, 1),
            (TestFailurePoint::WorkerRegistration, 2),
            (TestFailurePoint::WorkerInitialization, 2),
        ] {
            let worker = Probe {
                calls: AtomicUsize::new(0),
                value: AtomicUsize::new(0),
            };
            let parent = Probe {
                calls: AtomicUsize::new(0),
                value: AtomicUsize::new(0),
            };
            let (worker_descriptor, parent_descriptor) = descriptors(&worker, &parent);
            fault::set_test_failure(point);
            let failed_before = fault::test_resource_counts();
            // SAFETY: both descriptors refer to stack probes that remain live
            // through the joined operation; the test callback ABI is exact.
            let status = unsafe { run_tasks(worker_descriptor, parent_descriptor) };
            let failed_after = fault::test_resource_counts();
            assert_eq!(status, expected, "failure point did not classify portably");
            fault::clear_test_failure();
            assert_failure_resource_balance(failed_before, failed_after, point);
            assert_eq!(worker.calls.load(Ordering::Acquire), 0);
            assert_eq!(worker.value.load(Ordering::Acquire), 0);
            assert_eq!(parent.calls.load(Ordering::Acquire), 0);
            assert_eq!(parent.value.load(Ordering::Acquire), 0);
            assert_registry_capacity_is_restored();

            let retry_before = fault::test_resource_counts();
            let (worker_descriptor, parent_descriptor) = descriptors(&worker, &parent);
            assert_eq!(
                unsafe { run_tasks(worker_descriptor, parent_descriptor) },
                0,
                "resources leaked after injected {point:?} failure"
            );
            let retry_after = fault::test_resource_counts();
            assert_eq!(retry_after.slot_acquires - retry_before.slot_acquires, 1);
            assert_eq!(retry_after.slot_releases - retry_before.slot_releases, 1);
            assert_eq!(
                retry_after.stack_allocations - retry_before.stack_allocations,
                1
            );
            assert_eq!(retry_after.stack_destroys - retry_before.stack_destroys, 1);
            assert_eq!(
                retry_after.stack_registrations - retry_before.stack_registrations,
                1
            );
            assert_eq!(retry_after.mutex_inits - retry_before.mutex_inits, 1);
            assert_eq!(retry_after.mutex_destroys - retry_before.mutex_destroys, 1);
            assert_eq!(retry_after.condvar_inits - retry_before.condvar_inits, 1);
            assert_eq!(
                retry_after.condvar_destroys - retry_before.condvar_destroys,
                1
            );
            assert_eq!(
                retry_after.pthread_creates - retry_before.pthread_creates,
                1
            );
            assert_eq!(retry_after.pthread_joins - retry_before.pthread_joins, 1);
            assert_eq!(worker.calls.load(Ordering::Acquire), 1);
            assert_eq!(worker.value.load(Ordering::Acquire), 101);
            assert_eq!(parent.calls.load(Ordering::Acquire), 1);
            assert_eq!(parent.value.load(Ordering::Acquire), 202);
        }
    }
}
