//! Registry mapping SDK-generated IDs to `PlacementFilter` impls.
//!
//! Phase 7 of the SDK plan exposes custom `PlacementFilter`
//! callbacks across the FFI boundary. The SDKs already ship the
//! "build a `RegisteredPlacementFilter { id, fn }` from a closure"
//! surface (`placementFilterFromFn` in TS/Python/Go); what's
//! missing on the substrate side is a registry that maps the
//! SDK-generated `id` to an `Arc<dyn PlacementFilter>` so binding
//! code can resolve an ID to a filter impl before calling
//! scheduler methods.
//!
//! Layered separation of concerns:
//!
//! - **Substrate trait** (`PlacementFilter::placement_score`) —
//!   no FFI types appear in the trait surface.
//! - **Binding glue** — wraps a TSFN / `Py<PyAny>` / cgo function
//!   pointer in a `struct PlacementFilterCallback` that
//!   implements `PlacementFilter`. The binding owns the
//!   cross-thread invocation mechanics (TSFN's mpsc, `Python::attach`
//!   for the GIL, cgo trampoline for Go).
//! - **This registry** — stores `Arc<dyn PlacementFilter>` keyed
//!   by SDK ID, lookup-on-demand from scheduler-invoking code.
//!
//! Process-wide singleton via [`global_placement_filter_registry`]
//! so multiple `NetAdapter` instances share the registry. SDK IDs
//! are namespaced by the SDK helpers (`pf-N` counter +
//! optionally explicit IDs) so collisions across adapters are an
//! SDK concern, not a substrate one. If multi-tenant requires
//! per-adapter isolation, plumb a `PlacementFilterRegistry`
//! through `NetAdapter::shared` instead — the per-instance type
//! is the same.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use super::placement::PlacementFilter;

/// Per-id registration record. Bundles the filter impl with a
/// `binding` label so per-binding observability counters can
/// attribute invocations correctly without each binding having to
/// thread a separate label through every call site.
struct RegisteredEntry {
    filter: Arc<dyn PlacementFilter>,
    binding: String,
}

/// In-memory registry of `Arc<dyn PlacementFilter>` keyed by an
/// SDK-generated ID. Thread-safe (`DashMap`) so binding code can
/// register / lookup / unregister concurrently without locking
/// the entire registry.
///
/// Cloned references via [`Self::get`] keep the filter alive even
/// after `unregister` removes it from the map — the caller's
/// `Arc` clone ensures any in-flight scheduler call can still
/// score the held filter to completion. New scheduler calls
/// looking up the same ID will get `None`.
///
/// SDK Phase 7 polish: every successful [`Self::get`] increments a
/// per-binding invocation counter (Prometheus-friendly:
/// `dataforts_placement_callback_invocations_total{binding}`).
/// Bindings call [`Self::invocations_by_binding`] to read the
/// counters during scrape.
pub struct PlacementFilterRegistry {
    filters: DashMap<String, RegisteredEntry>,
    /// Per-binding invocation counter. Keyed by the `binding`
    /// label supplied at register-time. Atomic increments on every
    /// `get()` hit; bindings read via
    /// [`Self::invocations_by_binding`] without locking.
    invocations: DashMap<String, AtomicU64>,
}

impl std::fmt::Debug for PlacementFilterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn PlacementFilter` doesn't require `Debug` (the trait
        // surface is locked); print the count + IDs only.
        let ids: Vec<String> = self
            .filters
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        f.debug_struct("PlacementFilterRegistry")
            .field("len", &ids.len())
            .field("ids", &ids)
            .finish()
    }
}

impl PlacementFilterRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            filters: DashMap::new(),
            invocations: DashMap::new(),
        }
    }

    /// Register `filter` under `id` with `binding` as the
    /// observability label. Returns `true` on success, `false` if
    /// `id` is already registered (no overwrite — the SDK is
    /// responsible for generating unique IDs via its `pf-N`
    /// counter or explicit ID parameter).
    ///
    /// `binding` should be a stable short label identifying the
    /// SDK that owns the filter (e.g. `"node"`, `"python"`,
    /// `"go"`). It is used as the `binding=` Prometheus label on
    /// the `dataforts_placement_callback_invocations_total`
    /// counter.
    ///
    /// Refusing to overwrite avoids a class of bugs where two
    /// independently-built filters with the same SDK-supplied ID
    /// silently shadow each other; the SDK's
    /// `placementFilterFromFn` already generates monotonically
    /// unique IDs by default.
    pub fn register(
        &self,
        id: String,
        filter: Arc<dyn PlacementFilter>,
        binding: impl Into<String>,
    ) -> bool {
        let binding = binding.into();
        // CR-20: Pre-create the per-binding counter so reads from
        // `invocations_by_binding` see `0` for newly-registered
        // bindings rather than missing keys.
        //
        // Use `entry().or_insert_with()` instead of contains+insert
        // so concurrent registers of the same new binding don't
        // race: the previous shape had T1 and T2 both pass the
        // `contains_key == false` check, both `insert` a fresh
        // `AtomicU64::new(0)`, and the second insert overwrite
        // whatever count the first had already accumulated via
        // interleaved `get()` calls. Single atomic op now.
        self.invocations
            .entry(binding.clone())
            .or_insert_with(|| AtomicU64::new(0));
        match self.filters.entry(id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(RegisteredEntry { filter, binding });
                true
            }
        }
    }

    /// Look up the filter registered under `id`. Returns a cloned
    /// `Arc` so the caller can hold a `&dyn PlacementFilter` for
    /// the duration of a scheduler call without keeping the
    /// registry's internal lock.
    ///
    /// Side effect: increments the per-binding invocation counter
    /// for the entry's `binding` label. Misses (`None`) do NOT
    /// increment.
    pub fn get(&self, id: &str) -> Option<Arc<dyn PlacementFilter>> {
        let entry = self.filters.get(id)?;
        // Increment the per-binding counter. Pre-created at
        // register-time so the entry must exist; defensively
        // handle the missing-counter path with `or_insert_with`.
        if let Some(counter) = self.invocations.get(&entry.binding) {
            counter.fetch_add(1, Ordering::Relaxed);
        } else {
            self.invocations
                .entry(entry.binding.clone())
                .or_insert_with(|| AtomicU64::new(0))
                .fetch_add(1, Ordering::Relaxed);
        }
        Some(entry.filter.clone())
    }

    /// Drop the registration. Returns `true` if `id` was present.
    /// Existing `Arc` clones returned by `get` keep the filter
    /// alive until they're dropped — see the type docs.
    ///
    /// The per-binding invocation counter is NOT reset — counters
    /// are cumulative across the process lifetime, matching
    /// Prometheus counter semantics. Operators see
    /// rate-of-change, not absolute values, so retaining the
    /// counter across re-registrations is the correct shape.
    pub fn unregister(&self, id: &str) -> bool {
        self.filters.remove(id).is_some()
    }

    /// Whether `id` is currently registered.
    pub fn contains(&self, id: &str) -> bool {
        self.filters.contains_key(id)
    }

    /// Number of registered filters. Cheap (DashMap snapshot).
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// True when no filters are registered.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Drop every registration AND reset every invocation counter.
    /// Test-only — production callers should `unregister`
    /// deliberately and read counters incrementally.
    pub fn clear(&self) {
        self.filters.clear();
        self.invocations.clear();
    }

    /// Read the cumulative invocation count for a single binding.
    /// Returns `0` for unknown labels (pre-created entries return
    /// their actual count, possibly `0` if no `get()` has fired).
    pub fn invocation_count(&self, binding: &str) -> u64 {
        self.invocations
            .get(binding)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Snapshot the per-binding invocation counters.
    /// Format-friendly for the
    /// `dataforts_placement_callback_invocations_total{binding}`
    /// Prometheus counter — bindings render this map into their
    /// metrics endpoint.
    pub fn invocations_by_binding(&self) -> HashMap<String, u64> {
        self.invocations
            .iter()
            .map(|r| (r.key().clone(), r.value().load(Ordering::Relaxed)))
            .collect()
    }
}

impl Default for PlacementFilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_REGISTRY: OnceLock<PlacementFilterRegistry> = OnceLock::new();

/// Process-wide singleton registry.
///
/// Bindings (Node, Python, Go) call this to register their
/// language-specific `PlacementFilter` wrappers; scheduler-
/// invoking code calls this to resolve an SDK-supplied ID to an
/// `Arc<dyn PlacementFilter>` before invoking a scheduler method.
///
/// Initializes lazily on first call.
pub fn global_placement_filter_registry() -> &'static PlacementFilterRegistry {
    GLOBAL_REGISTRY.get_or_init(PlacementFilterRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::placement::{Artifact, NodeId};

    /// Trivial filter for tests — returns a fixed score every call.
    struct FixedFilter(f32);

    impl PlacementFilter for FixedFilter {
        fn placement_score(&self, _: &NodeId, _: &Artifact<'_>) -> Option<f32> {
            Some(self.0)
        }
    }

    /// Register / get / unregister round-trip.
    #[test]
    fn register_and_get_returns_same_filter() {
        let reg = PlacementFilterRegistry::new();
        let filter: Arc<dyn PlacementFilter> = Arc::new(FixedFilter(0.7));

        assert!(reg.register("pf-1".into(), filter.clone(), "test"));
        assert_eq!(reg.len(), 1);
        assert!(reg.contains("pf-1"));

        let got = reg
            .get("pf-1")
            .expect("registered filter must be retrievable");
        // Score the retrieved filter to confirm it's the same impl.
        let req = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let opt = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &req,
            optional: &opt,
        };
        assert_eq!(got.placement_score(&0x1234, &artifact), Some(0.7));
    }

    /// Re-registration of an existing ID returns `false` and
    /// leaves the original filter untouched. Pin the no-overwrite
    /// contract.
    #[test]
    fn register_refuses_to_overwrite_existing_id() {
        let reg = PlacementFilterRegistry::new();
        let original: Arc<dyn PlacementFilter> = Arc::new(FixedFilter(0.5));
        let challenger: Arc<dyn PlacementFilter> = Arc::new(FixedFilter(0.9));

        assert!(reg.register("pf-1".into(), original, "test"));
        assert!(
            !reg.register("pf-1".into(), challenger, "test"),
            "second register call must report failure"
        );

        // Original is still active.
        let req = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let opt = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &req,
            optional: &opt,
        };
        let got = reg.get("pf-1").unwrap();
        assert_eq!(got.placement_score(&0x1234, &artifact), Some(0.5));
    }

    /// `unregister` removes the entry and returns `true`; a
    /// subsequent `unregister` for the same ID returns `false`.
    #[test]
    fn unregister_returns_true_only_on_first_call() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(0.3)), "test");

        assert!(reg.unregister("pf-1"));
        assert!(!reg.contains("pf-1"));
        assert!(reg.is_empty());

        // Idempotent on absent IDs.
        assert!(!reg.unregister("pf-1"));
    }

    /// An `Arc` clone returned by `get` keeps the filter alive
    /// after `unregister` — pin the safety guarantee that an
    /// in-flight scheduler call won't see the filter disappear
    /// mid-evaluation.
    #[test]
    fn get_clone_outlives_unregister() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(0.42)), "test");

        let held = reg.get("pf-1").expect("filter is registered");
        assert!(reg.unregister("pf-1"));
        assert!(!reg.contains("pf-1"));

        // Held clone is still valid and scoreable.
        let req = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let opt = crate::adapter::net::behavior::capability::CapabilitySet::default();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &req,
            optional: &opt,
        };
        assert_eq!(held.placement_score(&0x1234, &artifact), Some(0.42));
    }

    /// `get` for an unregistered ID returns `None`.
    #[test]
    fn get_unknown_id_returns_none() {
        let reg = PlacementFilterRegistry::new();
        assert!(reg.get("pf-missing").is_none());
    }

    /// `clear` empties the registry — pin test-isolation behavior.
    #[test]
    fn clear_drops_every_registration() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(0.1)), "test");
        reg.register("pf-2".into(), Arc::new(FixedFilter(0.2)), "test");
        reg.register("pf-3".into(), Arc::new(FixedFilter(0.3)), "test");
        assert_eq!(reg.len(), 3);

        reg.clear();
        assert_eq!(reg.len(), 0);
        assert!(reg.get("pf-1").is_none());
    }

    /// Concurrent registers from different threads — pins the
    /// thread-safety contract (`DashMap` backing). Each thread
    /// inserts under a unique key; final count equals the thread
    /// count.
    #[test]
    fn concurrent_registers_under_unique_keys_all_succeed() {
        let reg = Arc::new(PlacementFilterRegistry::new());
        let n = 16usize;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let reg = reg.clone();
                std::thread::spawn(move || {
                    let f: Arc<dyn PlacementFilter> = Arc::new(FixedFilter(i as f32 / n as f32));
                    assert!(reg.register(format!("pf-{i}"), f, "test"));
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.len(), n);
    }

    /// The global singleton returns the same instance across
    /// calls. Use a fresh ID prefix to avoid collisions with
    /// other tests that touch the singleton.
    #[test]
    fn global_singleton_is_shared_across_calls() {
        let reg_a = global_placement_filter_registry();
        let reg_b = global_placement_filter_registry();
        // Same allocation address.
        assert!(std::ptr::eq(reg_a, reg_b));

        let id = "pf-singleton-test-unique-key";
        assert!(reg_a.register(id.into(), Arc::new(FixedFilter(0.6)), "test"));
        assert!(reg_b.contains(id));
        // Cleanup so we don't leak state to other tests.
        reg_b.unregister(id);
    }

    // =====================================================================
    // SDK Phase 7 polish — invocation counters
    // (`dataforts_placement_callback_invocations_total{binding}`).
    // =====================================================================

    /// Every successful `get()` increments the per-binding
    /// counter for that filter's `binding` label. Misses do NOT
    /// increment.
    #[test]
    fn get_increments_per_binding_invocation_counter() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");

        // Pre-created at register time, so 0 is the initial value.
        assert_eq!(reg.invocation_count("node"), 0);

        let _ = reg.get("pf-1");
        let _ = reg.get("pf-1");
        let _ = reg.get("pf-1");
        assert_eq!(reg.invocation_count("node"), 3);

        // Misses don't move the counter.
        let _ = reg.get("pf-missing");
        assert_eq!(reg.invocation_count("node"), 3);
    }

    /// Counters keyed by `binding` label, NOT by id. Multiple ids
    /// under the same binding share the counter.
    #[test]
    fn invocation_counter_aggregates_across_ids_within_binding() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");
        reg.register("pf-2".into(), Arc::new(FixedFilter(0.5)), "node");
        reg.register("pf-py".into(), Arc::new(FixedFilter(0.7)), "python");

        let _ = reg.get("pf-1");
        let _ = reg.get("pf-1");
        let _ = reg.get("pf-2");
        let _ = reg.get("pf-py");

        // Node binding accumulates 3 calls (pf-1 ×2 + pf-2 ×1).
        assert_eq!(reg.invocation_count("node"), 3);
        // Python binding sees 1.
        assert_eq!(reg.invocation_count("python"), 1);
        // Unrelated binding stays at 0.
        assert_eq!(reg.invocation_count("go"), 0);
    }

    /// `invocations_by_binding()` returns a snapshot suitable for
    /// rendering the
    /// `dataforts_placement_callback_invocations_total{binding=…}`
    /// counter family. Includes pre-created `binding` labels even
    /// when no `get()` has fired yet.
    #[test]
    fn invocations_by_binding_returns_full_snapshot() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");
        reg.register("pf-2".into(), Arc::new(FixedFilter(1.0)), "python");
        reg.register("pf-3".into(), Arc::new(FixedFilter(1.0)), "go");

        let _ = reg.get("pf-1");
        let _ = reg.get("pf-1");
        let _ = reg.get("pf-2");

        let snap = reg.invocations_by_binding();
        assert_eq!(snap.get("node").copied(), Some(2));
        assert_eq!(snap.get("python").copied(), Some(1));
        // Pre-created at register time even without invocations.
        assert_eq!(snap.get("go").copied(), Some(0));
    }

    /// `unregister` does NOT reset counters — Prometheus counter
    /// semantics: monotonic increase across the process lifetime.
    /// Operators see rate-of-change, not absolute values, so a
    /// re-registration cycle keeping the counter intact is the
    /// correct shape.
    #[test]
    fn unregister_preserves_invocation_counters() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");
        let _ = reg.get("pf-1");
        let _ = reg.get("pf-1");
        assert_eq!(reg.invocation_count("node"), 2);

        reg.unregister("pf-1");
        assert_eq!(
            reg.invocation_count("node"),
            2,
            "counter must survive unregister (cumulative semantics)",
        );

        // Re-register the same id; counter accumulates further.
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");
        let _ = reg.get("pf-1");
        assert_eq!(
            reg.invocation_count("node"),
            3,
            "counter must accumulate across re-registrations",
        );
    }

    /// `clear()` resets counters (test-isolation contract). Pin
    /// the documented exception to "counters are cumulative".
    #[test]
    fn clear_resets_invocation_counters() {
        let reg = PlacementFilterRegistry::new();
        reg.register("pf-1".into(), Arc::new(FixedFilter(1.0)), "node");
        let _ = reg.get("pf-1");
        assert_eq!(reg.invocation_count("node"), 1);

        reg.clear();
        assert_eq!(
            reg.invocation_count("node"),
            0,
            "clear() resets counters for test isolation",
        );
    }
}
