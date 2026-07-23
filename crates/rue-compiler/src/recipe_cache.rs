//! The recipe cache: a physical optimization in front of the authoritative
//! query terminals (RUE-1091, ADR-0066 §4).
//!
//! The cache is **not a dependency authority**. An entry is selected only after
//! the requesting body observes the exact current query terminal for that fact,
//! so cache identity is `(query family, logical key, exact terminal
//! incarnation/stamp)`. A stale terminal can never make an entry valid: a
//! request that presents a differing incarnation or stamp misses and rebuilds,
//! and a body never records an edge to a cache fingerprint. Entries hold only
//! owned recipe data — never a lease — so evicting one drops it with no retained
//! root left behind.
//!
//! The container is created once in constant work and fills on exact demand; its
//! constructor takes no declaration universe and so cannot enumerate one.
//! Concurrent misses for one exact recipe claim-or-join a single construction
//! while unrelated keys build concurrently, and **no shard lock is ever held
//! across a recipe construction or a nested request**: the shard lock only
//! find-or-creates the per-key cell and is released before `build()` runs.
//!
//! Accounting distinguishes an ordinary first build from rebuilding an
//! equal-stamp fact whose prior terminal incarnation was evicted and rederived
//! (the stamp-ABA case guarded by the incarnation field, mirroring
//! `rue_query::Observation`'s `incarnation`/`stamp`). The latter is
//! correctness-neutral and is metered separately as retention-induced recipe
//! thrash.
//!
//! Slice 2c keeps this inert to production: no production path constructs a
//! cache. It is exercised only by the test-only overlay path, so every counter
//! reads zero on the production path. The counters are exposed through the
//! session's unstable surface, mirroring the `parse_*` counters.

#![allow(dead_code)] // RUE-1091 slice 2c is inert: driven by the test-only overlay path.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::declaration_recipe::{DeclarationRecipe, RecipeLogicalKey};

/// The exact terminal incarnation/stamp a request presents when consulting the
/// cache. It mirrors the identity a dependent observes from the query runtime
/// (`rue_query::Observation`): a red/green publication `stamp` plus an opaque
/// session-local `incarnation` that prevents stamp-ABA across eviction and
/// rederivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TerminalStamp {
    /// Opaque session-local node incarnation. Two rederivations of one logical
    /// fact receive distinct incarnations even when their stamp is equal.
    pub incarnation: u64,
    /// Red/green terminal publication stamp. Equal-valued rederivations share a
    /// stamp.
    pub stamp: u64,
}

impl TerminalStamp {
    pub(crate) fn new(incarnation: u64, stamp: u64) -> Self {
        Self { incarnation, stamp }
    }
}

/// A recipe construction on this thread requested a recipe this thread is already
/// building (directly or transitively). Such a same-thread request would park
/// forever on the building cell's condvar, so the cache detects it and returns
/// this deterministic error instead of deadlocking.
///
/// This guard covers same-thread cycles only. A CROSS-thread cycle — T1 building
/// K then requesting J while T2 builds J then requests K — is NOT detected. That
/// is deliberate and out of contract: recipe construction converts an
/// already-computed durable artifact and must not request another recipe's build,
/// so a build-time cross-recipe request cannot arise on the production path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecipeBuildCycle {
    pub key: RecipeLogicalKey,
}

thread_local! {
    /// The `(cache id, logical key)` pairs this thread is currently building.
    /// Scoped per cache so two independent caches on one thread never alias, and
    /// per thread so an ordinary cross-thread join is never mistaken for a cycle.
    static IN_PROGRESS: std::cell::RefCell<std::collections::HashSet<(u64, RecipeLogicalKey)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// RAII guard recording that this thread is building `(cache, key)`. Entry fails
/// with [`RecipeBuildCycle`] if the pair is already in progress on this thread;
/// otherwise the pair is registered and removed again when the guard drops, on
/// every exit path (return, error, or panic).
struct InProgressGuard {
    cache: u64,
    key: RecipeLogicalKey,
}

impl InProgressGuard {
    fn enter(cache: u64, key: &RecipeLogicalKey) -> Result<Self, RecipeBuildCycle> {
        IN_PROGRESS.with(|set| {
            let mut set = set.borrow_mut();
            if !set.insert((cache, key.clone())) {
                return Err(RecipeBuildCycle { key: key.clone() });
            }
            Ok(Self {
                cache,
                key: key.clone(),
            })
        })
    }
}

impl Drop for InProgressGuard {
    fn drop(&mut self) {
        IN_PROGRESS.with(|set| {
            set.borrow_mut().remove(&(self.cache, self.key.clone()));
        });
    }
}

/// Cumulative recipe-cache metering (RUE-1121 counters). Lives on the cache
/// where the operations happen; the session holds one shared handle so the
/// unstable surface can read it. Reads zero on the production path because no
/// production path constructs a cache.
#[derive(Debug, Default)]
pub(crate) struct RecipeCacheCounters {
    entries_built: AtomicU64,
    entries_reused: AtomicU64,
    entries_evicted: AtomicU64,
    thrash_rebuilds: AtomicU64,
    containers_created: AtomicU64,
    containers_reused: AtomicU64,
}

impl RecipeCacheCounters {
    pub(crate) fn snapshot(&self) -> RecipeCacheMetrics {
        RecipeCacheMetrics {
            entries_built: self.entries_built.load(Ordering::Relaxed),
            entries_reused: self.entries_reused.load(Ordering::Relaxed),
            entries_evicted: self.entries_evicted.load(Ordering::Relaxed),
            thrash_rebuilds: self.thrash_rebuilds.load(Ordering::Relaxed),
            containers_created: self.containers_created.load(Ordering::Relaxed),
            containers_reused: self.containers_reused.load(Ordering::Relaxed),
        }
    }
}

/// An owned snapshot of the recipe-cache metering (RUE-1091, ADR-0066 §4). Every
/// field reads zero on the production path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecipeCacheMetrics {
    /// Recipe entries built for the first time (ordinary builds).
    pub entries_built: u64,
    /// Exact hits: an entry selected because its terminal incarnation and stamp
    /// matched the request exactly.
    pub entries_reused: u64,
    /// Entries independently evicted from the cache.
    pub entries_evicted: u64,
    /// Equal-stamp rebuilds forced solely because the prior terminal incarnation
    /// was evicted and rederived (retention-induced thrash), reported separately
    /// from `entries_built`.
    pub thrash_rebuilds: u64,
    /// Cache containers constructed.
    pub containers_created: u64,
    /// Requests that reused an already-constructed container.
    pub containers_reused: u64,
}

/// The build state of one per-key cell.
#[derive(Debug)]
enum CellState {
    /// No entry is present. `CellSlot::last_terminal` may carry the shadow of a
    /// previously built terminal so an in-place same-stamp/different-incarnation
    /// rederivation is metered as thrash.
    Vacant,
    /// One thread is constructing the entry; joiners wait on `Cell::ready`.
    Building,
    /// A built entry keyed to the exact terminal that produced it.
    Built {
        recipe: Arc<DeclarationRecipe>,
        terminal: TerminalStamp,
    },
    /// The cell has been removed from its shard map and is no longer canonical.
    /// A Phase-2 arrival that still holds this stale `Arc<Cell>` retries from
    /// Phase 1, where it finds (or creates) the current canonical cell.
    Retired,
}

#[derive(Debug)]
struct CellSlot {
    state: CellState,
    /// The full terminal of the most recently built entry. Thrash is defined
    /// verbatim by §4 as an equal-stamp rebuild whose prior terminal incarnation
    /// was evicted and rederived, so the shadow keeps incarnation AND stamp: a
    /// rebuild is thrash only when the stamp is equal and the incarnation differs.
    /// (A cache-only miss that presents the same terminal is an ordinary rebuild,
    /// never thrash.) Under independent eviction the whole cell is removed, so the
    /// shadow is dropped with it; §4's thrash meter tracks retention-induced
    /// rebuilds of RETAINED keys, and a post-removal rebuild is an ordinary build.
    last_terminal: Option<TerminalStamp>,
}

/// One per-logical-key build cell. Its own lock and condvar coordinate
/// claim-or-join **without** holding any shard lock, and the lock is dropped
/// across `build()`.
///
/// A DISTINCT-key nested request from within a build cannot deadlock: it targets
/// a different cell and the shard lock is not held across construction. A
/// SAME-thread request for a key this thread is already building (same key or a
/// transitive cycle) is caught by [`InProgressGuard`] and returns
/// [`RecipeBuildCycle`] rather than parking. A CROSS-thread build-time cycle is
/// not detected and is out of contract (see `RecipeBuildCycle`).
#[derive(Debug)]
struct Cell {
    slot: Mutex<CellSlot>,
    ready: Condvar,
}

impl Cell {
    fn new() -> Self {
        Self {
            slot: Mutex::new(CellSlot {
                state: CellState::Vacant,
                last_terminal: None,
            }),
            ready: Condvar::new(),
        }
    }
}

#[derive(Debug, Default)]
struct Shard {
    cells: HashMap<RecipeLogicalKey, Arc<Cell>>,
}

/// Distinguishes cache instances so the per-thread in-progress set never aliases
/// one cache's key against another's.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

/// The recipe cache container. Sharded so unrelated keys never contend on one
/// lock; each key's construction runs under its own cell with no shard lock held.
#[derive(Debug)]
pub(crate) struct RecipeCache {
    id: u64,
    shards: Vec<Mutex<Shard>>,
    counters: Arc<RecipeCacheCounters>,
    /// Cumulative Phase-1 retries forced by a stale cell observing `Retired`
    /// (test-only observability for the eviction/removal race).
    #[cfg(test)]
    retired_retries: AtomicU64,
    /// A cell injected as the result of the FIRST Phase-1 pass (test-only), used
    /// to drive the stale-`Arc<Cell>` observes-`Retired` retry deterministically
    /// without threads.
    #[cfg(test)]
    injected_first_cell: Mutex<Option<Arc<Cell>>>,
}

impl RecipeCache {
    const DEFAULT_SHARDS: usize = 16;

    /// Construct a cache in constant work. The constructor takes no declaration
    /// universe and enumerates nothing; it only allocates the fixed shard array.
    pub(crate) fn new(counters: Arc<RecipeCacheCounters>) -> Self {
        counters.containers_created.fetch_add(1, Ordering::Relaxed);
        Self::empty(counters, Self::DEFAULT_SHARDS)
    }

    #[cfg(test)]
    pub(crate) fn with_shards(counters: Arc<RecipeCacheCounters>, shards: usize) -> Self {
        counters.containers_created.fetch_add(1, Ordering::Relaxed);
        Self::empty(counters, shards.max(1))
    }

    fn empty(counters: Arc<RecipeCacheCounters>, shards: usize) -> Self {
        let shards = (0..shards.max(1))
            .map(|_| Mutex::new(Shard::default()))
            .collect();
        Self {
            id: NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            shards,
            counters,
            #[cfg(test)]
            retired_retries: AtomicU64::new(0),
            #[cfg(test)]
            injected_first_cell: Mutex::new(None),
        }
    }

    /// Note that a body request reused this already-constructed container.
    pub(crate) fn note_container_reused(&self) {
        self.counters
            .containers_reused
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A snapshot of this cache's metering.
    pub(crate) fn metrics(&self) -> RecipeCacheMetrics {
        self.counters.snapshot()
    }

    /// The number of entries currently represented (in `Built` state).
    pub(crate) fn represented(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .cells
                    .values()
                    .filter(|cell| {
                        matches!(
                            cell.slot
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .state,
                            CellState::Built { .. }
                        )
                    })
                    .count()
            })
            .sum()
    }

    /// The number of cells retained across all shards (test-only). Independent
    /// eviction removes a cell entirely, so this stays bounded by the live key set
    /// rather than growing with every key ever requested.
    #[cfg(test)]
    pub(crate) fn cell_count(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .cells
                    .len()
            })
            .sum()
    }

    /// Cumulative Phase-1 retries forced by a stale cell observing `Retired`.
    #[cfg(test)]
    pub(crate) fn retired_retries(&self) -> u64 {
        self.retired_retries.load(Ordering::Relaxed)
    }

    /// The current canonical cell for `key` (test-only), so a test can hold a
    /// stale `Arc<Cell>` across an eviction and drive the `Retired` retry.
    #[cfg(test)]
    fn current_cell(&self, key: &RecipeLogicalKey) -> Arc<Cell> {
        self.phase1(key)
    }

    /// Arrange for the next `get_or_build` to receive `cell` from its FIRST Phase-1
    /// pass (test-only), deterministically driving the stale-cell `Retired` retry.
    #[cfg(test)]
    fn inject_first_cell(&self, cell: Arc<Cell>) {
        *self
            .injected_first_cell
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(cell);
    }

    fn shard_for(&self, key: &RecipeLogicalKey) -> &Mutex<Shard> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let index = (hasher.finish() as usize) % self.shards.len();
        &self.shards[index]
    }

    /// Find-or-create the canonical per-key cell under the shard lock, releasing
    /// the shard lock before returning so no shard lock is held across a
    /// construction or a nested request.
    fn phase1(&self, key: &RecipeLogicalKey) -> Arc<Cell> {
        // Test-only injection: return a caller-supplied (possibly Retired) cell on
        // the first pass to drive the stale-cell retry deterministically.
        #[cfg(test)]
        if let Some(cell) = self
            .injected_first_cell
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return cell;
        }
        let mut shard = self
            .shard_for(key)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        shard
            .cells
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Cell::new()))
            .clone()
    }

    /// Select the entry for `(key, terminal)` if it is present with the exact
    /// terminal incarnation and stamp; otherwise build it. Concurrent requests
    /// for one exact key claim-or-join a single construction; a request that
    /// presents a differing incarnation or stamp misses and rebuilds. No shard
    /// lock — and no cell lock — is held while `build` runs.
    ///
    /// Returns [`RecipeBuildCycle`] if this thread requests a key it is already
    /// building (same key or a transitive same-thread cycle) instead of parking
    /// on the building cell's condvar. Cross-thread build-time cycles are not
    /// detected and are out of contract (see [`RecipeBuildCycle`]).
    pub(crate) fn get_or_build<F>(
        &self,
        key: RecipeLogicalKey,
        terminal: TerminalStamp,
        build: F,
    ) -> Result<Arc<DeclarationRecipe>, RecipeBuildCycle>
    where
        F: FnOnce() -> DeclarationRecipe,
    {
        // Reentrancy guard: a build closure on this thread that (directly or
        // transitively) requests a key this thread is already building would park
        // forever on the condvar below. Detect it and error deterministically.
        let _cycle_guard = InProgressGuard::enter(self.id, &key)?;

        let mut build = Some(build);
        // Phase-1 retry loop: a stale cell that observes `Retired` re-runs Phase 1
        // to find the current canonical cell. Bounded — the canonical cell is
        // found (or created) on the next pass.
        loop {
            let cell = self.phase1(&key);

            // Phase 2: claim-or-join on the cell. The cell lock is released across
            // `build()`, so a DISTINCT-key nested request cannot deadlock and
            // concurrent joiners wait on the condvar. (The `unwrap_or_else`
            // poison recovery below is defensive for a hypothetical panic=unwind
            // world; under this toolchain's panic=abort these paths are dead. On
            // unwind a builder panicking mid-`build` would strand joiners in
            // `Building` and would need a reset-on-panic guard, out of scope here.)
            let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
            let outcome = loop {
                match &slot.state {
                    CellState::Built {
                        recipe,
                        terminal: have,
                    } if *have == terminal => {
                        let recipe = recipe.clone();
                        drop(slot);
                        self.counters.entries_reused.fetch_add(1, Ordering::Relaxed);
                        return Ok(recipe);
                    }
                    CellState::Built { terminal: have, .. } => {
                        // A stale terminal cannot select the entry. Thrash is an
                        // equal-stamp rebuild of a RETAINED entry whose incarnation
                        // was rederived; a changed stamp is an ordinary rebuild.
                        let thrash = have.stamp == terminal.stamp
                            && have.incarnation != terminal.incarnation;
                        slot.state = CellState::Building;
                        // Release the cell lock before building: no cell lock is
                        // held across `build()`, and `finish_build` re-locks it.
                        drop(slot);
                        break BuildDecision::Build(thrash);
                    }
                    CellState::Building => {
                        slot = cell
                            .ready
                            .wait(slot)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    CellState::Vacant => {
                        // A fresh cell has no shadow (ordinary build). An in-place
                        // shadow, if present, is thrash only on equal-stamp AND
                        // different-incarnation — a same-terminal cache miss is
                        // an ordinary rebuild.
                        let thrash = matches!(
                            slot.last_terminal,
                            Some(prev)
                                if prev.stamp == terminal.stamp
                                    && prev.incarnation != terminal.incarnation
                        );
                        slot.state = CellState::Building;
                        drop(slot);
                        break BuildDecision::Build(thrash);
                    }
                    CellState::Retired => {
                        // This stale `Arc<Cell>` was removed from its shard map.
                        // Retry Phase 1 to reach the current canonical cell.
                        drop(slot);
                        break BuildDecision::Retry;
                    }
                }
            };

            match outcome {
                BuildDecision::Build(thrash) => {
                    let build = build.take().expect("build closure consumed at most once");
                    return Ok(self.finish_build(&cell, terminal, thrash, build));
                }
                BuildDecision::Retry => {
                    #[cfg(test)]
                    self.retired_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Run `build` with no lock held, then publish the entry and meter it.
    fn finish_build<F>(
        &self,
        cell: &Cell,
        terminal: TerminalStamp,
        thrash: bool,
        build: F,
    ) -> Arc<DeclarationRecipe>
    where
        F: FnOnce() -> DeclarationRecipe,
    {
        let recipe = Arc::new(build());
        {
            let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.state = CellState::Built {
                recipe: recipe.clone(),
                terminal,
            };
            slot.last_terminal = Some(terminal);
            cell.ready.notify_all();
        }
        if thrash {
            self.counters
                .thrash_rebuilds
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.entries_built.fetch_add(1, Ordering::Relaxed);
        }
        recipe
    }

    /// Independently evict the entry for `key`, dropping its owned recipe AND
    /// removing the cell from its shard map so retention stays bounded by the live
    /// key set. The removed cell is flipped to `Retired` first, so any concurrent
    /// Phase-2 arrival holding the stale `Arc<Cell>` retries from Phase 1 onto the
    /// next canonical cell rather than resurrecting the removed one. No recipe and
    /// no retained root survive; the stamp shadow is dropped with the cell (a
    /// later rebuild is an ordinary build, matching §4's retained-key thrash
    /// definition). Returns whether an entry was present to evict.
    pub(crate) fn evict(&self, key: &RecipeLogicalKey) -> bool {
        let mut shard = self
            .shard_for(key)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(cell) = shard.cells.get(key) else {
            return false;
        };
        let cell = cell.clone();
        let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(slot.state, CellState::Built { .. }) {
            // Retire under the shard lock, then remove from the map: a stale
            // holder that later locks this cell observes `Retired` and retries.
            slot.state = CellState::Retired;
            drop(slot);
            shard.cells.remove(key);
            drop(shard);
            self.counters
                .entries_evicted
                .fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

/// Whether a Phase-2 pass decided to build (with a thrash flag) or must retry
/// Phase 1 because it observed a retired cell.
enum BuildDecision {
    Build(bool),
    Retry,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::body_overlay::{BodySemanticOverlay, LocalToken};
    use crate::canonical_semantic::BodyOutcomeResolver;
    use crate::declaration_recipe::DefinitionRecipe;
    use crate::durable_semantics::{
        DurableDeclarationPayload, DurableDeclarationSemantic, DurableType,
    };
    use crate::{ModuleId, StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace};

    fn recipe(name: &str) -> DeclarationRecipe {
        let key = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("pkg/main.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            name,
            None,
        );
        DeclarationRecipe::Definition(Box::new(DefinitionRecipe::from_durable(
            &DurableDeclarationSemantic {
                key,
                is_public: true,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            },
            &[],
        )))
    }

    fn cache() -> RecipeCache {
        RecipeCache::new(Arc::default())
    }

    // Exact-selection discipline: an entry is served only when the caller presents
    // the exact (family, logical key, incarnation, stamp). A differing stamp or
    // incarnation MISSES. The miss with equal stamp but different incarnation is
    // metered as thrash (stamp-ABA), never as an ordinary build.
    #[test]
    fn exact_terminal_selection_only_serves_an_exact_match() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();

        // First presentation: ordinary build.
        let first = cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 100), || subject.clone())
            .unwrap();
        assert_eq!(cache.metrics().entries_built, 1);
        assert_eq!(cache.metrics().entries_reused, 0);

        // Exact same terminal: hit, no rebuild.
        let again = cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 100), || {
                panic!("must not build")
            })
            .unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "exact terminal must reuse the entry"
        );
        assert_eq!(cache.metrics().entries_reused, 1);
        assert_eq!(cache.metrics().entries_built, 1);

        // Different stamp (fact genuinely changed): ordinary rebuild, not thrash.
        cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 200), || subject.clone())
            .unwrap();
        assert_eq!(cache.metrics().entries_built, 2);
        assert_eq!(cache.metrics().thrash_rebuilds, 0);

        // Equal stamp, different incarnation (rederived terminal, still retained):
        // thrash, and the ordinary-build counter does NOT move.
        cache
            .get_or_build(key.clone(), TerminalStamp::new(2, 200), || subject.clone())
            .unwrap();
        assert_eq!(
            cache.metrics().entries_built,
            2,
            "thrash must not count as an ordinary build"
        );
        assert_eq!(cache.metrics().thrash_rebuilds, 1);

        // Equal stamp AND equal incarnation: this is the exact entry now (I2,S200),
        // so it is a reuse — never a thrash rebuild and never an ordinary build.
        cache
            .get_or_build(key, TerminalStamp::new(2, 200), || panic!("must not build"))
            .unwrap();
        assert_eq!(cache.metrics().entries_reused, 2);
        assert_eq!(cache.metrics().entries_built, 2);
        assert_eq!(cache.metrics().thrash_rebuilds, 1);
    }

    // Concurrent misses for one exact key claim-or-join exactly one construction;
    // both requests receive the same entry. The joiner's build closure never runs.
    #[test]
    fn concurrent_same_key_builds_once_and_both_join() {
        let cache = Arc::new(cache());
        let subject = recipe("apply");
        let key = subject.logical_key();
        let terminal = TerminalStamp::new(1, 7);

        let builds = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();

        // Thread A wins the cell and builds; it blocks inside `build` until the
        // main thread confirms thread B is joining.
        let a = {
            let (cache, subject, key, builds) =
                (cache.clone(), subject.clone(), key.clone(), builds.clone());
            std::thread::spawn(move || {
                cache.get_or_build(key, terminal, move || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    subject.clone()
                })
            })
        };

        // Wait until A is inside `build` (state is Building, no lock held).
        started_rx.recv().unwrap();

        // Thread B joins the in-flight construction; its closure must never run.
        let joiner_ran = Arc::new(AtomicBool::new(false));
        let b = {
            let (cache, subject, key, joiner_ran) = (
                cache.clone(),
                subject.clone(),
                key.clone(),
                joiner_ran.clone(),
            );
            std::thread::spawn(move || {
                cache.get_or_build(key, terminal, move || {
                    joiner_ran.store(true, Ordering::SeqCst);
                    subject.clone()
                })
            })
        };

        // Give B time to reach the condvar wait, then release A.
        std::thread::sleep(Duration::from_millis(30));
        go_tx.send(()).unwrap();

        let first = a.join().unwrap().unwrap();
        let second = b.join().unwrap().unwrap();

        assert_eq!(builds.load(Ordering::SeqCst), 1, "exactly one construction");
        assert!(
            !joiner_ran.load(Ordering::SeqCst),
            "the joiner must not build"
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "both requests get the same entry"
        );
        assert_eq!(cache.metrics().entries_built, 1);
        assert_eq!(cache.metrics().entries_reused, 1);
    }

    // Two threads missing on DIFFERENT keys build concurrently: neither construction
    // excludes the other. Each build closure waits until both are simultaneously
    // in flight (with a timeout escape), proving no mutual exclusion through a
    // global lock.
    #[test]
    fn concurrent_different_keys_build_concurrently() {
        let cache = Arc::new(cache());
        let in_build = Arc::new(AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicBool::new(false));

        let spawn = |name: &'static str| {
            let (cache, in_build, overlapped) =
                (cache.clone(), in_build.clone(), overlapped.clone());
            std::thread::spawn(move || {
                let subject = recipe(name);
                cache
                    .get_or_build(subject.logical_key(), TerminalStamp::new(1, 1), move || {
                        // Announce this construction and wait (bounded) for the
                        // other to be in flight too. If a global lock serialized
                        // the two builds, this one would run alone and never
                        // observe a peer.
                        in_build.fetch_add(1, Ordering::SeqCst);
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while in_build.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                            std::thread::yield_now();
                        }
                        if in_build.load(Ordering::SeqCst) >= 2 {
                            overlapped.store(true, Ordering::SeqCst);
                        }
                        in_build.fetch_sub(1, Ordering::SeqCst);
                        subject.clone()
                    })
                    .unwrap()
            })
        };

        let a = spawn("alpha");
        let b = spawn("beta");
        a.join().unwrap();
        b.join().unwrap();

        assert!(
            overlapped.load(Ordering::SeqCst),
            "both builds must be in flight simultaneously (no global lock serializes them)"
        );
        assert_eq!(cache.metrics().entries_built, 2);
    }

    // No lock spans a construction: a build closure that itself performs a second
    // cache operation on ANOTHER key (placed in the SAME shard) must not deadlock.
    #[test]
    fn nested_request_from_within_a_build_does_not_deadlock() {
        // One shard forces the outer and nested keys to share a shard, so a shard
        // lock held across construction would deadlock here.
        let cache = Arc::new(RecipeCache::with_shards(Arc::default(), 1));
        let outer = recipe("outer");
        let outer_key = outer.logical_key();
        let inner = recipe("inner");
        let inner_key = inner.logical_key();

        let built = {
            let inner_cache = cache.clone();
            let inner = inner.clone();
            let inner_key = inner_key.clone();
            let inner_probe = inner.logical_key();
            cache
                .get_or_build(outer_key.clone(), TerminalStamp::new(1, 1), move || {
                    // Nested cache operation on ANOTHER key from within
                    // construction: a distinct key on the same thread is not a
                    // cycle and cannot deadlock.
                    let nested = inner_cache
                        .get_or_build(inner_key, TerminalStamp::new(1, 1), || inner.clone())
                        .unwrap();
                    assert_eq!(nested.logical_key(), inner_probe);
                    outer.clone()
                })
                .unwrap()
        };

        assert_eq!(built.logical_key(), outer_key);
        assert_eq!(
            cache.metrics().entries_built,
            2,
            "both outer and nested entries built"
        );
        // The nested entry is really present and independently selectable.
        assert!(cache.evict(&inner_key));
    }

    // Lifetime: evicting the backing terminal drops the entry's owned recipe AND
    // removes the cell entirely, so no retained root and no unbounded shadow are
    // left behind — the recipe is unreachable afterward, the represented and cell
    // counts return to zero, and a later rebuild is an ordinary build.
    #[test]
    fn eviction_drops_the_entry_without_leaking_a_root() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();

        let handle = cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 1), || subject.clone())
            .unwrap();
        let weak = Arc::downgrade(&handle);
        drop(handle);
        assert_eq!(cache.represented(), 1);
        assert_eq!(cache.cell_count(), 1);
        assert!(
            weak.upgrade().is_some(),
            "the cache retains the only strong ref"
        );

        assert!(cache.evict(&key), "the present entry is evicted");
        assert_eq!(cache.metrics().entries_evicted, 1);
        assert_eq!(
            cache.represented(),
            0,
            "nothing is represented after eviction"
        );
        assert_eq!(
            cache.cell_count(),
            0,
            "eviction removes the cell entirely — no unbounded retention"
        );
        assert!(
            weak.upgrade().is_none(),
            "eviction drops the recipe: no unleased retained root remains"
        );

        // A rebuild after removal is an ORDINARY build: the shadow was dropped with
        // the cell, and §4's thrash meter tracks retention-induced rebuilds of
        // RETAINED keys only.
        cache
            .get_or_build(key, TerminalStamp::new(2, 1), || subject.clone())
            .unwrap();
        assert_eq!(cache.metrics().thrash_rebuilds, 0);
        assert_eq!(
            cache.metrics().entries_built,
            2,
            "a post-removal rebuild is an ordinary build, not thrash"
        );
        assert_eq!(cache.cell_count(), 1);
    }

    // A rebuild after removal that presents the SAME terminal (same stamp AND same
    // incarnation) is an ordinary build, never miscounted as thrash: a cache-only
    // miss of a still-live terminal is not retention-induced thrash.
    #[test]
    fn rebuild_after_removal_with_the_same_terminal_is_ordinary() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();
        let terminal = TerminalStamp::new(9, 9);

        cache
            .get_or_build(key.clone(), terminal, || subject.clone())
            .unwrap();
        assert!(cache.evict(&key));
        cache
            .get_or_build(key, terminal, || subject.clone())
            .unwrap();

        assert_eq!(
            cache.metrics().thrash_rebuilds,
            0,
            "same-terminal is not thrash"
        );
        assert_eq!(cache.metrics().entries_built, 2);
    }

    // Constant-work container creation: constructing the cache does no
    // per-declaration work. A large synthetic universe is available but the
    // constructor cannot enumerate it (it takes no universe), and no entry is
    // built or represented.
    #[test]
    fn container_creation_does_not_enumerate_the_universe() {
        // A 1000-declaration universe wrapped so any read is observable.
        let probes = Arc::new(AtomicUsize::new(0));
        let universe: Vec<DeclarationRecipe> =
            (0..1000).map(|i| recipe(&format!("decl_{i}"))).collect();
        let touched = probes.clone();
        let _peek = move |i: usize| {
            touched.fetch_add(1, Ordering::SeqCst);
            universe[i].clone()
        };

        let counters = Arc::<RecipeCacheCounters>::default();
        let cache = RecipeCache::new(counters);

        assert_eq!(cache.metrics().containers_created, 1);
        assert_eq!(
            cache.metrics().entries_built,
            0,
            "construction builds nothing"
        );
        assert_eq!(cache.represented(), 0, "construction represents nothing");
        assert_eq!(
            probes.load(Ordering::SeqCst),
            0,
            "construction touches no declaration in the universe"
        );
    }

    // Container created-once / reused-many accounting through a shared meter.
    #[test]
    fn container_creation_and_reuse_are_metered() {
        let meter = Arc::<RecipeCacheCounters>::default();
        let cache = RecipeCache::new(meter.clone());
        assert_eq!(meter.snapshot().containers_created, 1);
        assert_eq!(meter.snapshot().containers_reused, 0);
        cache.note_container_reused();
        cache.note_container_reused();
        assert_eq!(meter.snapshot().containers_created, 1);
        assert_eq!(meter.snapshot().containers_reused, 2);
    }

    // Overlay integration: the cache stores recipes, never tokens. Two overlays
    // that fill from the SAME cached recipe each mint their own distinct local
    // token while recording the same stable identity; the recipe is built once
    // and reused on the second fill.
    #[test]
    fn two_overlays_fill_from_one_cached_recipe_with_distinct_tokens() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();
        let terminal = TerminalStamp::new(1, 1);

        let mut first = BodySemanticOverlay::new();
        let mut second = BodySemanticOverlay::new();
        assert_ne!(first.issuer(), second.issuer());

        let first_token = first
            .import_via_cache(&cache, key.clone(), terminal, || subject.clone())
            .unwrap();
        // The second overlay fills from the cache, which now reuses the built
        // recipe rather than rebuilding it.
        let second_token = second
            .import_via_cache(&cache, key, terminal, || {
                panic!("the cached recipe must be reused, not rebuilt")
            })
            .unwrap();

        assert_eq!(cache.metrics().entries_built, 1, "recipe built once");
        assert_eq!(
            cache.metrics().entries_reused,
            1,
            "reused for the second overlay"
        );
        assert_ne!(
            first_token, second_token,
            "two overlays sharing one cached recipe mint distinct local tokens"
        );

        // Both tokens resolve to the same stable identity through each overlay's
        // own reverse map — the recipe carried no token.
        let LocalToken::Definition(first_def) = first_token else {
            panic!("a definition recipe mints a definition token")
        };
        let LocalToken::Definition(second_def) = second_token else {
            panic!("a definition recipe mints a definition token")
        };
        assert_eq!(
            first.key_for_semantic_token(first_def).unwrap(),
            second.key_for_semantic_token(second_def).unwrap(),
        );
    }

    // Reentrancy (F1): a build closure that requests the SAME key it is building on
    // this thread returns a deterministic cycle error instead of parking forever on
    // the building cell's condvar. The reviewer confirmed the pre-fix Building-wait
    // self-deadlocked here.
    #[test]
    fn reentrant_same_key_request_errors_deterministically() {
        let cache = Arc::new(cache());
        let subject = recipe("apply");
        let key = subject.logical_key();

        let nested_cache = cache.clone();
        let nested_key = key.clone();
        let result = cache.get_or_build(key.clone(), TerminalStamp::new(1, 1), move || {
            // Same key, same thread: must error, not deadlock.
            let reentrant =
                nested_cache.get_or_build(nested_key.clone(), TerminalStamp::new(1, 1), || {
                    panic!("a reentrant same-key request must not build")
                });
            assert_eq!(reentrant, Err(RecipeBuildCycle { key: nested_key }));
            subject.clone()
        });

        assert!(result.is_ok(), "the outer build still completes");
        assert_eq!(cache.metrics().entries_built, 1);
    }

    // Reentrancy (F1): a two-key same-thread cycle — build(K) requests J, build(J)
    // requests K — is detected at the innermost request and errors deterministically
    // rather than deadlocking. Single-shard so both keys share a shard.
    #[test]
    fn two_key_same_thread_cycle_errors_deterministically() {
        let cache = Arc::new(RecipeCache::with_shards(Arc::default(), 1));
        let k = recipe("k");
        let k_key = k.logical_key();
        let j = recipe("j");
        let j_key = j.logical_key();

        let cache_j = cache.clone();
        let cache_k = cache.clone();
        let j_key_outer = j_key.clone();
        let k_key_inner = k_key.clone();
        let result = cache.get_or_build(k_key.clone(), TerminalStamp::new(1, 1), move || {
            // build K -> request J
            let j_result = cache_j.get_or_build(j_key_outer, TerminalStamp::new(1, 1), move || {
                // build J -> request K (already building on this thread): cycle.
                let k_result =
                    cache_k.get_or_build(k_key_inner.clone(), TerminalStamp::new(1, 1), || {
                        panic!("the transitive same-thread cycle must not build")
                    });
                assert_eq!(k_result, Err(RecipeBuildCycle { key: k_key_inner }));
                j.clone()
            });
            assert!(
                j_result.is_ok(),
                "J still builds; only the cycle back to K errors"
            );
            k.clone()
        });

        assert!(result.is_ok());
        assert_eq!(cache.metrics().entries_built, 2, "K and J both build");
    }

    // Eviction removal (F2): after evicting a key, the next request rebuilds exactly
    // once through a fresh canonical cell (the removed cell is gone).
    #[test]
    fn evicted_key_rebuilds_once_via_a_fresh_cell() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();

        cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 1), || subject.clone())
            .unwrap();
        assert!(cache.evict(&key));
        assert_eq!(cache.cell_count(), 0);

        let builds = AtomicUsize::new(0);
        cache
            .get_or_build(key.clone(), TerminalStamp::new(2, 2), || {
                builds.fetch_add(1, Ordering::SeqCst);
                subject.clone()
            })
            .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1, "exactly one rebuild");
        assert_eq!(cache.metrics().entries_built, 2);
        assert_eq!(cache.cell_count(), 1, "the fresh canonical cell is present");
    }

    // Eviction race (F2): a Phase-2 arrival that still holds a stale `Arc<Cell>`
    // observes `Retired` and retries Phase 1 onto the current canonical cell,
    // building exactly once. Driven deterministically via first-cell injection —
    // no threads or timing.
    #[test]
    fn stale_cell_observing_retired_retries_onto_the_canonical_cell() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();

        // Build so a real cell exists, then capture it as a soon-to-be-stale handle.
        cache
            .get_or_build(key.clone(), TerminalStamp::new(1, 1), || subject.clone())
            .unwrap();
        let stale = cache.current_cell(&key);

        // Evict: retires `stale` and removes it from the shard map.
        assert!(cache.evict(&key));
        assert_eq!(cache.cell_count(), 0);

        // The next request receives the stale (Retired) cell from its first Phase-1
        // pass; it must retry onto a fresh canonical cell and build exactly once.
        cache.inject_first_cell(stale);
        let builds = AtomicUsize::new(0);
        let built = cache
            .get_or_build(key.clone(), TerminalStamp::new(2, 2), || {
                builds.fetch_add(1, Ordering::SeqCst);
                subject.clone()
            })
            .unwrap();

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "exactly one build via the fresh cell"
        );
        assert_eq!(
            cache.retired_retries(),
            1,
            "the stale cell forced exactly one Phase-1 retry"
        );
        assert_eq!(
            cache.cell_count(),
            1,
            "only the fresh canonical cell remains"
        );
        assert_eq!(built.logical_key(), key);
    }

    // Eviction removal (F2): repeated evict/request cycles keep the shard map bounded
    // by the LIVE key set, not by the number of keys ever requested.
    #[test]
    fn repeated_evict_and_request_keep_the_shard_map_bounded() {
        let cache = cache();
        let subject = recipe("apply");
        let key = subject.logical_key();

        for i in 0..64 {
            cache
                .get_or_build(key.clone(), TerminalStamp::new(i, i), || subject.clone())
                .unwrap();
            assert_eq!(cache.cell_count(), 1);
            assert!(cache.evict(&key));
            assert_eq!(cache.cell_count(), 0, "the cell is removed, not retained");
        }
    }
}
