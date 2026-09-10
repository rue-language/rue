//! Owner-driven teardown of one query runtime.
//!
//! A runtime's owner ends it at a point it chooses rather than waiting for the
//! last reference to the core, because that moment does not reliably arrive:
//! an evaluator graph is free to hold `QueryFamily` and `QueryRuntime` handles
//! in cycles, and a family holds its runtime's core. Teardown releases the two
//! resources an owner gave the runtime — its physical worker threads
//! (RUE-2043) and the values registered here (RUE-2072) — so dropping the owner
//! actually frees the memo tables and the core.

use std::fmt;
use std::sync::{Arc, RwLock, Weak};

use crate::{read, write};

/// A value the runtime empties when its owner tears the runtime down.
///
/// Implementors are registered with [`QueryRuntime::release_at_teardown`](
/// crate::QueryRuntime::release_at_teardown) and held there weakly, so
/// registration never itself keeps a value alive.
pub trait ReleaseOnTeardown: Send + Sync {
    /// Drops whatever this value holds. Called once, by the runtime's owner,
    /// when no request can be in flight; it must leave the value usable and
    /// observably empty rather than poisoned.
    fn release_on_teardown(&self);
}

/// Emptying a mutex-guarded value replaces it with its default.
///
/// The compiler's session-held publication roots are exactly this shape: an
/// `Arc<Mutex<..>>` shared between the database and the registered evaluators
/// that publish into it, holding retained terminal pins which each own a
/// family handle.
impl<T: Default + Send> ReleaseOnTeardown for std::sync::Mutex<T> {
    fn release_on_teardown(&self) {
        let held = std::mem::take(&mut *crate::lock(self));
        drop(held);
    }
}

/// A value a registered evaluator reads which is installed only after that
/// evaluator exists.
///
/// Registration builds families in one pass, so every edge that runs backwards
/// in that order — a family whose evaluator reads a family constructed later,
/// or reads itself — is closed by installing the value here once construction
/// has reached it. Each installed value holds strong family handles and a
/// family holds its runtime's core, so these holders are precisely the
/// reference cycles no owner field can break. Emptying them at teardown is
/// what lets a dropped session free its memo tables and its core.
///
/// A read after teardown returns `None`, so the evaluator refuses with a typed
/// abort instead of touching a released value.
pub struct LateBound<T: Send + Sync + 'static> {
    slot: RwLock<Slot<T>>,
}

enum Slot<T> {
    /// Construction has not reached the installation site yet.
    Pending,
    /// Behind an `Arc` so a reader clones one reference and lets the lock go.
    /// The value is read from inside a running evaluator, which may recursively
    /// enter the same family; holding a read guard across that work would be a
    /// recursive read lock, and teardown is not worth that hazard.
    Installed(Arc<T>),
    /// The owner tore the runtime down. Distinguished from `Pending` so a
    /// post-teardown installation is refused rather than resurrecting a cycle.
    Released,
}

impl<T: Send + Sync + 'static> fmt::Debug for LateBound<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match &*read(&self.slot) {
            Slot::Pending => "pending",
            Slot::Installed(_) => "installed",
            Slot::Released => "released",
        };
        formatter
            .debug_struct("LateBound")
            .field("state", &state)
            .finish()
    }
}

impl<T: Send + Sync + 'static> LateBound<T> {
    pub(crate) fn new() -> Self {
        Self {
            slot: RwLock::new(Slot::Pending),
        }
    }

    /// Installs the back-patched value.
    ///
    /// Returns the value unchanged when one is already installed, or when the
    /// runtime has been torn down. Mirrors `OnceLock::set`, so an installation
    /// site keeps asserting that it ran exactly once.
    pub fn set(&self, value: T) -> Result<(), T> {
        let mut slot = write(&self.slot);
        match &*slot {
            Slot::Pending => {
                *slot = Slot::Installed(Arc::new(value));
                Ok(())
            }
            Slot::Installed(_) | Slot::Released => Err(value),
        }
    }

    /// The installed value, or `None` once the owner released it.
    ///
    /// `None` before installation is a registration defect; `None` after
    /// teardown is a request against a runtime whose owner is finished with
    /// it. Both are refusals rather than panics: a caller which reached either
    /// state must abort its request, never proceed on a released value.
    pub fn get(&self) -> Option<Arc<T>> {
        match &*read(&self.slot) {
            Slot::Installed(value) => Some(value.clone()),
            Slot::Pending | Slot::Released => None,
        }
    }
}

impl<T: Send + Sync + 'static> ReleaseOnTeardown for LateBound<T> {
    fn release_on_teardown(&self) {
        // Take the value out from under the lock and drop it afterwards: the
        // released value owns family handles whose destructors run retention
        // bookkeeping, and none of that work may run while this holder's lock
        // is held.
        let held = std::mem::replace(&mut *write(&self.slot), Slot::Released);
        drop(held);
    }
}

/// Values registered for release when the runtime's owner tears it down.
///
/// Weak, because every registered value is owned by the graph it is part of.
/// A registry that owned them would be one more edge keeping the cycle alive.
#[derive(Debug, Default)]
pub(crate) struct TeardownRegistry {
    entries: std::sync::Mutex<Vec<Weak<dyn ReleaseOnTeardown>>>,
}

impl TeardownRegistry {
    pub(crate) fn register(&self, value: Weak<dyn ReleaseOnTeardown>) {
        crate::lock(&self.entries).push(value);
    }

    /// Empties every still-live registered value, exactly once.
    ///
    /// The registry is drained first so a released value's destructor cannot
    /// observe a registration that is about to be released again.
    pub(crate) fn release(&self) {
        let entries: Vec<_> = crate::lock(&self.entries).drain(..).collect();
        for entry in entries {
            if let Some(value) = entry.upgrade() {
                value.release_on_teardown();
            }
        }
    }

    pub(crate) fn registered(&self) -> usize {
        crate::lock(&self.entries).len()
    }
}

/// A non-owning handle on a query runtime.
///
/// Upgrading answers whether anything still holds the runtime's core. That is
/// the question a session-retention test asks: after the owning database is
/// dropped, a runtime whose graph freed itself has no core left to upgrade to,
/// while one still held by an evaluator cycle does (RUE-2072).
#[derive(Debug, Clone)]
pub struct WeakQueryRuntime {
    pub(crate) core: Weak<crate::RuntimeCore>,
}

impl WeakQueryRuntime {
    /// The runtime, while its core is still referenced.
    pub fn upgrade(&self) -> Option<crate::QueryRuntime> {
        Some(crate::QueryRuntime {
            core: self.core.upgrade()?,
        })
    }

    /// References to the core held right now, zero once it is freed.
    ///
    /// Exposed for retention accounting: a census of what still holds a core
    /// is otherwise invisible to a test that must not itself hold one.
    pub fn strong_count(&self) -> usize {
        Weak::strong_count(&self.core)
    }
}

/// Wraps `value` so the runtime empties it at teardown.
pub(crate) fn erase<T: ReleaseOnTeardown + 'static>(value: &Arc<T>) -> Weak<dyn ReleaseOnTeardown> {
    let erased: Arc<dyn ReleaseOnTeardown> = value.clone();
    Arc::downgrade(&erased)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryRuntime;

    #[test]
    fn late_bound_values_are_released_by_runtime_teardown() {
        let runtime = QueryRuntime::new(1);
        let holder = runtime.late_bound::<Arc<u32>>();
        let value = Arc::new(7_u32);
        assert!(holder.set(value.clone()).is_ok());
        assert_eq!(Arc::strong_count(&value), 2);
        assert_eq!(**holder.get().expect("the value is installed"), 7);

        runtime.shut_down();

        assert!(
            holder.get().is_none(),
            "a read after teardown must report the value as gone, not serve it"
        );
        assert_eq!(
            Arc::strong_count(&value),
            1,
            "teardown must drop the held value, not merely hide it"
        );
        assert!(
            holder.set(Arc::new(9)).is_err(),
            "a released holder must not accept a fresh value and rebuild the cycle"
        );
    }

    #[test]
    fn a_mutex_registered_for_teardown_is_emptied_in_place() {
        let runtime = QueryRuntime::new(1);
        let root = Arc::new(std::sync::Mutex::new(vec![Arc::new(1_u8)]));
        runtime.release_at_teardown(&root);
        let held = crate::lock(&root)[0].clone();
        assert_eq!(Arc::strong_count(&held), 2);

        runtime.shut_down();

        assert!(crate::lock(&root).is_empty());
        assert_eq!(Arc::strong_count(&held), 1);
    }

    #[test]
    fn teardown_releases_each_registration_once_and_drops_the_registry() {
        let runtime = QueryRuntime::new(1);
        let root = Arc::new(std::sync::Mutex::new(vec![1_u8]));
        runtime.release_at_teardown(&root);
        let dropped = runtime.late_bound::<u8>();
        assert!(dropped.set(3).is_ok());
        assert_eq!(runtime.registered_teardown_values(), 2);

        runtime.shut_down();
        assert_eq!(
            runtime.registered_teardown_values(),
            0,
            "teardown consumes its registrations"
        );
        // A second teardown is a no-op rather than a double release.
        crate::lock(&root).push(9);
        runtime.shut_down();
        assert_eq!(crate::lock(&root).as_slice(), [9]);
    }

    #[derive(Debug, Clone, PartialEq, Eq, std::hash::Hash)]
    struct Key(&'static str);

    impl crate::QueryKey for Key {
        fn stable_identity(&self) -> String {
            self.0.to_owned()
        }

        fn stable_hash(&self, hasher: &mut crate::StableHasher) {
            std::hash::Hash::hash(self.0, hasher);
        }
    }

    #[test]
    fn a_request_reading_a_released_holder_aborts_instead_of_serving() {
        let runtime = QueryRuntime::new(1);
        let holder = runtime.late_bound::<u64>();
        assert!(holder.set(41).is_ok());
        let installed = holder.clone();
        let family = runtime
            .family_with_evaluator("test.late-bound", 4, move |_, _, _: &Key| {
                let value = installed.get().ok_or(crate::QueryAbort::ForeignRuntime)?;
                Ok(crate::QueryOutput::success(*value + 1))
            })
            .expect("the family has one canonical name");
        runtime
            .publish_revision(crate::Revision::new(1, 1), [])
            .expect("the revision publishes");
        let served = runtime
            .request_registered(
                &family,
                crate::Revision::new(1, 1),
                Key("k"),
                crate::CancellationToken::new(),
            )
            .into_result()
            .expect("an installed holder serves the request");
        assert_eq!(served.outcome(), &crate::QueryOutcome::Success(42));

        runtime.shut_down();

        let refused = runtime
            .request_registered(
                &family,
                crate::Revision::new(1, 1),
                Key("fresh"),
                crate::CancellationToken::new(),
            )
            .into_result()
            .expect_err("a released holder must refuse the request rather than serve it");
        assert_eq!(refused, crate::QueryAbort::ForeignRuntime);
    }

    #[test]
    fn a_runtime_with_no_families_is_freed_when_its_owner_drops_it() {
        let runtime = QueryRuntime::new(1);
        let weak = runtime.downgrade();
        assert_eq!(weak.strong_count(), 1);
        runtime.shut_down();
        drop(runtime);
        assert!(weak.upgrade().is_none());
        assert_eq!(weak.strong_count(), 0);
    }
}
