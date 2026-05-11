//! `BlobAdapterRegistry` — process-wide map from `adapter_id` to
//! the registered `Arc<dyn BlobAdapter>`. Mirrors the shape of
//! `behavior::placement_registry::PlacementFilterRegistry` so
//! bindings can reuse the same registration pattern.

use std::sync::{Arc, OnceLock};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use super::adapter::BlobAdapter;

/// Errors returned by [`BlobAdapterRegistry::register`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum BlobAdapterRegistryError {
    /// An adapter with the same id is already registered. Call
    /// [`BlobAdapterRegistry::unregister`] first.
    DuplicateId(String),
}

impl std::fmt::Display for BlobAdapterRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "blob adapter id already registered: {}", id),
        }
    }
}

impl std::error::Error for BlobAdapterRegistryError {}

/// Process-wide registry of blob adapters. Cloned references via
/// [`Self::get`] keep an adapter alive even after `unregister`
/// removes it from the map — an in-flight fetch still gets to
/// complete against the held `Arc`.
pub struct BlobAdapterRegistry {
    adapters: DashMap<String, Arc<dyn BlobAdapter>>,
}

impl std::fmt::Debug for BlobAdapterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<String> = self
            .adapters
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        f.debug_struct("BlobAdapterRegistry")
            .field("len", &ids.len())
            .field("ids", &ids)
            .finish()
    }
}

impl BlobAdapterRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self {
            adapters: DashMap::new(),
        }
    }

    /// Register `adapter` under its `adapter_id()`. Returns
    /// `Err(DuplicateId)` when an entry already exists.
    pub fn register(
        &self,
        adapter: Arc<dyn BlobAdapter>,
    ) -> Result<(), BlobAdapterRegistryError> {
        let id = adapter.adapter_id().to_owned();
        match self.adapters.entry(id.clone()) {
            Entry::Occupied(_) => Err(BlobAdapterRegistryError::DuplicateId(id)),
            Entry::Vacant(slot) => {
                slot.insert(adapter);
                Ok(())
            }
        }
    }

    /// Remove the entry at `id`. Returns the removed adapter, or
    /// `None` when no such entry existed.
    pub fn unregister(&self, id: &str) -> Option<Arc<dyn BlobAdapter>> {
        self.adapters.remove(id).map(|(_, v)| v)
    }

    /// Lookup; returns a cloned `Arc` so the caller's reference
    /// outlives a concurrent `unregister`.
    pub fn get(&self, id: &str) -> Option<Arc<dyn BlobAdapter>> {
        self.adapters.get(id).map(|r| r.value().clone())
    }

    /// Count of currently-registered adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// True iff [`Self::len`] is 0.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Snapshot of registered ids. Cheap; copies strings under a
    /// brief read.
    pub fn ids(&self) -> Vec<String> {
        self.adapters
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }
}

impl Default for BlobAdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_REGISTRY: OnceLock<BlobAdapterRegistry> = OnceLock::new();

/// Process-wide singleton.
pub fn global_blob_adapter_registry() -> &'static BlobAdapterRegistry {
    GLOBAL_REGISTRY.get_or_init(BlobAdapterRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::noop::NoopAdapter;

    #[test]
    fn register_get_unregister_round_trips() {
        let reg = BlobAdapterRegistry::new();
        let a: Arc<dyn BlobAdapter> = Arc::new(NoopAdapter::new("test-one"));
        reg.register(a.clone()).unwrap();
        assert_eq!(reg.len(), 1);
        let fetched = reg.get("test-one").unwrap();
        assert_eq!(fetched.adapter_id(), "test-one");
        let removed = reg.unregister("test-one").unwrap();
        assert_eq!(removed.adapter_id(), "test-one");
        assert!(reg.is_empty());
    }

    #[test]
    fn duplicate_registration_rejected() {
        let reg = BlobAdapterRegistry::new();
        let a: Arc<dyn BlobAdapter> = Arc::new(NoopAdapter::new("dup"));
        let b: Arc<dyn BlobAdapter> = Arc::new(NoopAdapter::new("dup"));
        reg.register(a).unwrap();
        let err = reg.register(b).unwrap_err();
        assert_eq!(err, BlobAdapterRegistryError::DuplicateId("dup".into()));
    }

    #[test]
    fn unregister_returns_none_when_missing() {
        let reg = BlobAdapterRegistry::new();
        assert!(reg.unregister("ghost").is_none());
    }

    #[test]
    fn get_after_unregister_returns_none_but_prior_handle_lives() {
        let reg = BlobAdapterRegistry::new();
        let a: Arc<dyn BlobAdapter> = Arc::new(NoopAdapter::new("liveness"));
        reg.register(a).unwrap();
        let held = reg.get("liveness").unwrap();
        let removed = reg.unregister("liveness").unwrap();
        // The held + removed handles both keep the adapter alive.
        assert_eq!(held.adapter_id(), "liveness");
        assert_eq!(removed.adapter_id(), "liveness");
        // Subsequent lookups miss.
        assert!(reg.get("liveness").is_none());
    }

    #[test]
    fn ids_snapshot_lists_registered() {
        let reg = BlobAdapterRegistry::new();
        reg.register(Arc::new(NoopAdapter::new("a"))).unwrap();
        reg.register(Arc::new(NoopAdapter::new("b"))).unwrap();
        let mut ids = reg.ids();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }
}
