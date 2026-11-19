//! Node-owned service registrations for the language bindings
//! (`docs/internal/plans/APP_INTEGRATION_BLOCKERS_PLAN.md` Phase 4).
//! Binding glue, not SDK API: Rust callers own their `ServeHandle`s
//! directly (RAII), which is idiomatic there. Twin of the Node binding's
//! `bindings/node/src/keepalive.rs`; it isn't shared through `net_sdk`
//! because the bindings' base `net`/`cortex` builds don't depend on the
//! SDK. Keep the two in step.
//!
//! A binding's serve handle (Node `rpc.serve`, `serveStreaming`,
//! `serveEnrollmentAuto`, org and A2A serves, and the Python twins) is a
//! garbage-collected object. It used to own its registration outright,
//! so an app that didn't keep the handle referenced lost the service at
//! whatever GC collected it — immediately in Python, at some random
//! later point in Node — silently. Now the NODE owns it: each registration is also
//! held here, keyed by its node, until the handle's `close()` / `stop()`
//! or the node's `shutdown()`. Finalizing the JS wrapper releases
//! nothing.
//!
//! Keyed by the node's address. That is safe because every registration
//! held here keeps its node alive (a core `ServeHandle` owns an
//! `Arc<MeshNode>`), so a key can't be reused by another node while
//! entries for it remain; `shutdown` releases a node's entries before it
//! tries to take sole ownership of the node.
//!
//! The cost of the trade, by design: a mesh that is dropped without
//! `shutdown()` keeps its services (and so its node) until the process
//! exits, the same as `serveBlobTransfer` always has.
//!
//! Shutdown is terminal for the registry too (PYBR-01). `release_all`
//! sweeps a node's entries once, but a binding facade that outlives the
//! mesh object (an async RPC wrapper owns its own `Arc<MeshNode>`) can
//! still register afterwards, including while the shutdown is running
//! off the interpreter lock. Such a registration used to land in a
//! freshly recreated bucket that nothing would ever sweep again: a
//! repeated `shutdown()` returns early, so the entry kept its service
//! and node alive for the life of the process. `release_all` now
//! records the node as shut down, and [`hold`] refuses a shut-down node
//! by releasing the registration on the spot; its handle reports itself
//! closed.
//!
//! This is the one deliberate difference from the Node twin. Node's
//! `shutdown()` needs sole ownership of the node and puts the node back
//! when it cannot get it, so a mesh can outlive a failed shutdown and
//! must keep accepting registrations; a registration that slips in
//! holds a node reference, which fails that shutdown, and the next one
//! sweeps it. Python's shutdown takes the node for good, so it needs
//! the fence.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use net::adapter::net::MeshNode;
use parking_lot::Mutex;

/// One held registration, type-erased.
trait Release: Send + Sync {
    fn release(&self);
}

/// A handle's shared slot, plus the runtime its contents must be
/// dropped on (tearing down a serve registration touches runtime-bound
/// resources, and release may run on the JS thread).
struct Entry<T> {
    slot: Arc<Mutex<Option<T>>>,
    runtime: Option<tokio::runtime::Handle>,
}

impl<T: Send> Release for Entry<T> {
    fn release(&self) {
        let _enter = self.runtime.as_ref().map(|r| r.enter());
        // Take under the lock, drop outside it.
        let taken = self.slot.lock().take();
        drop(taken);
    }
}

type NodeKey = usize;
type Held = HashMap<u64, Arc<dyn Release>>;

/// The registry proper, generic over the node type so its shutdown
/// fence is testable without building a mesh.
struct Registry<N> {
    held: HashMap<NodeKey, Held>,
    /// Nodes whose `release_all` has run. The `Weak` pins the node's
    /// allocation, so its address (the key) cannot be reused by a
    /// different node while the marker exists; a marker is pruned once
    /// its node is gone, when no registration against it can arrive.
    ///
    /// What a dead marker retains is the node's bare allocation (its
    /// fields are already dropped), and only until the next registry
    /// operation of any kind — admit, forget or shutdown all prune. So
    /// the retained memory is bounded by the nodes shut down and dropped
    /// since the registry was last touched.
    shut_down: HashMap<NodeKey, Weak<N>>,
}

impl<N> Default for Registry<N> {
    fn default() -> Self {
        Self {
            held: HashMap::new(),
            shut_down: HashMap::new(),
        }
    }
}

impl<N> Registry<N> {
    fn prune_shut_down(&mut self) {
        self.shut_down.retain(|_, node| node.strong_count() > 0);
    }

    /// Admit `entry` under `id`, or hand it back if `node` has shut down.
    fn admit(
        &mut self,
        node: &Arc<N>,
        id: u64,
        entry: Arc<dyn Release>,
    ) -> Option<Arc<dyn Release>> {
        self.prune_shut_down();
        let key = key_of(node);
        if self.shut_down.contains_key(&key) {
            return Some(entry);
        }
        self.held.entry(key).or_default().insert(id, entry);
        None
    }

    fn forget(&mut self, key: NodeKey, id: u64) {
        self.prune_shut_down();
        if let Some(held) = self.held.get_mut(&key) {
            held.remove(&id);
            if held.is_empty() {
                self.held.remove(&key);
            }
        }
    }

    /// Mark `node` shut down and take everything held for it.
    fn shut_down(&mut self, node: &Arc<N>) -> Option<Held> {
        self.prune_shut_down();
        let key = key_of(node);
        self.shut_down.insert(key, Arc::downgrade(node));
        self.held.remove(&key)
    }
}

fn registry() -> &'static Mutex<Registry<MeshNode>> {
    static REGISTRY: OnceLock<Mutex<Registry<MeshNode>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn key_of<N>(node: &Arc<N>) -> NodeKey {
    Arc::as_ptr(node) as NodeKey
}

/// A handle's place in the registry. Deliberately no `Drop`: the JS
/// wrapper is finalized at GC, and finalizing it must not release the
/// service. Only [`Self::forget`] (from `close()` / `stop()`) removes it.
pub(crate) struct Registration {
    node: NodeKey,
    id: u64,
}

impl Registration {
    /// Remove the entry without releasing it — the handle already
    /// released its slot itself. Idempotent.
    pub(crate) fn forget(&self) {
        registry().lock().forget(self.node, self.id);
    }
}

/// Hold `slot` for `node` until the handle forgets it or the node shuts
/// down. On a node that has already shut down, the slot is released
/// immediately instead, so the handle comes back closed.
pub(crate) fn hold<T: Send + 'static>(
    node: &Arc<MeshNode>,
    slot: &Arc<Mutex<Option<T>>>,
    runtime: Option<tokio::runtime::Handle>,
) -> Registration {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let entry: Arc<dyn Release> = Arc::new(Entry {
        slot: Arc::clone(slot),
        runtime,
    });
    let refused = registry().lock().admit(node, id, entry);
    // Released outside the registry lock, as in `release_all`.
    if let Some(entry) = refused {
        entry.release();
    }
    Registration {
        node: key_of(node),
        id,
    }
}

/// Release every registration held for `node` — `shutdown()` stops all
/// of a node's services — and refuse any registered after it. Releases
/// outside the registry lock: dropping a registration can take its own
/// locks.
pub(crate) fn release_all(node: &Arc<MeshNode>) {
    let held = registry().lock().shut_down(node);
    for entry in held.into_iter().flat_map(HashMap::into_values) {
        entry.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: &Arc<Mutex<Option<u8>>>) -> Arc<dyn Release> {
        Arc::new(Entry {
            slot: Arc::clone(slot),
            runtime: None,
        })
    }

    #[test]
    fn a_registration_after_shutdown_is_refused_not_held() {
        let mut reg = Registry::<u8>::default();
        let node = Arc::new(0u8);
        let before = Arc::new(Mutex::new(Some(1u8)));
        assert!(reg.admit(&node, 1, entry(&before)).is_none());

        let swept = reg.shut_down(&node).expect("the live entry is swept");
        assert_eq!(swept.len(), 1);

        // A facade that outlived the mesh registers again. It must not
        // recreate a bucket that no later shutdown would sweep.
        let after = Arc::new(Mutex::new(Some(2u8)));
        let refused = reg.admit(&node, 2, entry(&after));
        assert!(
            refused.is_some(),
            "a shut-down node must refuse a registration"
        );
        assert!(
            reg.held.is_empty(),
            "nothing may be held for a shut-down node"
        );
        refused.unwrap().release();
        assert!(
            after.lock().is_none(),
            "the refused registration is released"
        );
    }

    #[test]
    fn the_shutdown_marker_ends_with_its_node() {
        let mut reg = Registry::<u8>::default();
        let node = Arc::new(0u8);
        reg.shut_down(&node);
        assert_eq!(reg.shut_down.len(), 1);
        drop(node);

        // A new node may reuse the old address only once the marker is
        // gone, and the marker goes when its node does.
        let other = Arc::new(0u8);
        let slot = Arc::new(Mutex::new(Some(3u8)));
        assert!(reg.admit(&other, 3, entry(&slot)).is_none());
        assert!(reg.shut_down.is_empty());
        assert_eq!(reg.held.len(), 1);
    }

    #[test]
    fn closing_any_handle_prunes_a_dead_shutdown_marker() {
        let mut reg = Registry::<u8>::default();
        let node = Arc::new(0u8);
        reg.shut_down(&node);
        drop(node);
        assert_eq!(
            reg.shut_down.len(),
            1,
            "nothing has touched the registry yet"
        );
        // An unrelated handle closing is enough to release the allocation.
        reg.forget(0xdead, 1);
        assert!(reg.shut_down.is_empty());
    }
}
