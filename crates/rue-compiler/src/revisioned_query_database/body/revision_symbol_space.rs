//! Revision-scoped symbol equality space for materialized body RIR.
//!
//! The append-only interner generation is the sole authority for symbol
//! equality across bodies in one retained semantic revision.

use super::super::*;

/// The revision-scoped owner of the shared symbol equality space (ADR-0076).
///
/// One append-only interner serves every body of one semantic revision, so a
/// name in the program's nominal closure is interned once rather than once per
/// body. A generation is retired when its revision falls out of the window
/// below; a body still carrying the retired generation fails
/// `require_rir_authority` and re-runs, so a superseded equality space is never
/// silently reused.
///
/// The window holds more than one revision on purpose. Retiring the previous
/// generation on every mint would make two concurrently pinned revisions retire
/// each other's space and abandon each other's bodies without progressing;
/// keeping the recent few makes that need more simultaneously live revisions
/// than the engine pins, while still bounding how many interners are resident.
#[derive(Debug)]
pub(in crate::revisioned_query_database) struct RevisionSymbolSpace {
    live: Mutex<VecDeque<(Revision, rue_rir::SharedSymbolSpace)>>,
    generations: rue_rir::SymbolSpaceGenerations,
    max_entries: usize,
}

impl Default for RevisionSymbolSpace {
    fn default() -> Self {
        Self::with_owner_bound(rue_lexer::MAX_INTERNED_STRINGS)
    }
}

impl RevisionSymbolSpace {
    /// How many revisions' equality spaces stay live at once.
    pub(in crate::revisioned_query_database) const WINDOW: usize = 4;

    pub(in crate::revisioned_query_database) fn with_owner_bound(max_entries: usize) -> Self {
        Self {
            live: Mutex::new(VecDeque::new()),
            generations: rue_rir::SymbolSpaceGenerations::default(),
            max_entries,
        }
    }

    /// The live generation for `revision`, minting it if this revision has no
    /// live generation yet.
    pub(in crate::revisioned_query_database) fn generation(
        &self,
        revision: Revision,
    ) -> rue_rir::SharedSymbolSpace {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, space)) = live.iter().find(|(pinned, _)| *pinned == revision) {
            return space.clone();
        }
        let space = self
            .generations
            .next_generation_with_owner_bound(self.max_entries);
        live.push_back((revision, space.clone()));
        while live.len() > Self::WINDOW {
            if let Some((_, evicted)) = live.pop_front() {
                evicted.supersede();
            }
        }
        space
    }
}
