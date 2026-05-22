//! `MemoriesState` — the materialized view held behind the
//! `CortexAdapter<MemoriesState>`'s `RwLock`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::filter::MemoriesFilter;
use super::types::{Memory, MemoryId};

/// Materialized view over the memories log.
///
/// Per perf #96, the value type is `Arc<Memory>` (not `Memory`).
/// Query reads (`execute`, `find_many`, `MemoriesQuery::collect`)
/// hand back `Vec<Arc<Memory>>` — each match is an atomic
/// refcount bump instead of a deep `Memory` clone (which carries
/// `String content`, `Vec<String> tags`, `String source` — 3+
/// allocations per cloned entry). Mutations route through
/// `Arc::make_mut` so a unique Arc mutates in place (the common
/// case under the fold's serial write lock) and a shared Arc
/// (outstanding reader) clones-on-write to preserve the reader's
/// snapshot.
///
/// `Serialize` / `Deserialize` are derived so the state can be
/// snapshotted via [`super::super::CortexAdapter::snapshot`] and
/// restored via [`super::super::CortexAdapter::open_from_snapshot`].
/// `Arc<Memory>` serializes transparently with serde's `rc`
/// feature (already enabled in this crate's `serde` dep).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MemoriesState {
    pub(super) memories: HashMap<MemoryId, Arc<Memory>>,
}

impl MemoriesState {
    /// Create an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a memory by id. Returns a borrow into the `Arc`
    /// payload so the common single-read case doesn't pay a
    /// refcount bump; callers that need an owned share can clone
    /// the Arc themselves via [`Self::get_arc`].
    pub fn get(&self, id: MemoryId) -> Option<&Memory> {
        self.memories.get(&id).map(|a| a.as_ref())
    }

    /// Look up a memory by id and return an Arc share. Cheap
    /// (atomic refcount bump) — use when the caller needs to hold
    /// the memory past the borrow's lifetime.
    pub fn get_arc(&self, id: MemoryId) -> Option<Arc<Memory>> {
        self.memories.get(&id).cloned()
    }

    /// Total number of memories currently retained.
    pub fn len(&self) -> usize {
        self.memories.len()
    }

    /// True if no memories are retained.
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
    }

    /// True if a memory with `id` exists.
    pub fn contains(&self, id: MemoryId) -> bool {
        self.memories.contains_key(&id)
    }

    /// Iterate over every retained memory.
    pub fn all(&self) -> impl Iterator<Item = &Memory> {
        self.memories.values().map(|a| a.as_ref())
    }

    /// Iterate over currently-pinned memories.
    pub fn pinned(&self) -> impl Iterator<Item = &Memory> {
        self.memories
            .values()
            .map(|a| a.as_ref())
            .filter(|m| m.pinned)
    }

    /// Iterate over memories that are NOT pinned.
    pub fn unpinned(&self) -> impl Iterator<Item = &Memory> {
        self.memories
            .values()
            .map(|a| a.as_ref())
            .filter(|m| !m.pinned)
    }

    // -- Prisma-ish convenience surface (NetDB layer) -------------------

    /// Look up a memory by id. Alias of [`Self::get`].
    pub fn find_unique(&self, id: MemoryId) -> Option<&Memory> {
        self.get(id)
    }

    /// Collect all memories matching `filter`, respecting order +
    /// limit. Returns `Vec<Arc<Memory>>` per perf #96 — each
    /// match is one atomic refcount bump instead of the legacy
    /// deep `Memory` clone.
    pub fn find_many(&self, filter: &MemoriesFilter) -> Vec<Arc<Memory>> {
        filter.apply(self.query()).collect()
    }

    /// Count memories matching `filter`. Ignores `limit`.
    pub fn count_where(&self, filter: &MemoriesFilter) -> usize {
        filter.apply(self.query()).count()
    }

    /// True if any memory matches `filter`. Short-circuits.
    pub fn exists_where(&self, filter: &MemoriesFilter) -> bool {
        filter.apply(self.query()).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: MemoryId, pinned: bool) -> Arc<Memory> {
        Arc::new(Memory {
            id,
            content: format!("mem-{}", id),
            tags: Vec::new(),
            source: "test".into(),
            created_ns: 0,
            updated_ns: 0,
            pinned,
        })
    }

    #[test]
    fn test_empty_state() {
        let s = MemoriesState::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.get(1).is_none());
        assert!(!s.contains(1));
        assert_eq!(s.pinned().count(), 0);
        assert_eq!(s.unpinned().count(), 0);
    }

    #[test]
    fn test_pin_split() {
        let mut s = MemoriesState::new();
        s.memories.insert(1, mem(1, true));
        s.memories.insert(2, mem(2, false));
        s.memories.insert(3, mem(3, true));

        assert_eq!(s.len(), 3);
        assert_eq!(s.pinned().count(), 2);
        assert_eq!(s.unpinned().count(), 1);
        assert!(s.contains(2));
        assert!(!s.contains(99));
    }
}
