//! Node-owned service registrations for the language bindings
//! (`docs/internal/plans/APP_INTEGRATION_BLOCKERS_PLAN.md` Phase 4).
//! Binding glue, not SDK API: Rust callers own their `ServeHandle`s
//! directly (RAII), which is idiomatic there. The Python binding has a
//! twin (`bindings/python/src/keepalive.rs`); it isn't shared through
//! `net_sdk` because the bindings' base `net`/`cortex` builds don't
//! depend on the SDK. Keep the two in step.
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

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

fn registry() -> &'static Mutex<HashMap<NodeKey, Held>> {
    static REGISTRY: OnceLock<Mutex<HashMap<NodeKey, Held>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn key_of(node: &Arc<MeshNode>) -> NodeKey {
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
        let mut map = registry().lock();
        if let Some(held) = map.get_mut(&self.node) {
            held.remove(&self.id);
            if held.is_empty() {
                map.remove(&self.node);
            }
        }
    }
}

/// Hold `slot` for `node` until the handle forgets it or the node shuts
/// down.
pub(crate) fn hold<T: Send + 'static>(
    node: &Arc<MeshNode>,
    slot: &Arc<Mutex<Option<T>>>,
    runtime: Option<tokio::runtime::Handle>,
) -> Registration {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let key = key_of(node);
    let entry: Arc<dyn Release> = Arc::new(Entry {
        slot: Arc::clone(slot),
        runtime,
    });
    registry().lock().entry(key).or_default().insert(id, entry);
    Registration { node: key, id }
}

/// Release every registration held for `node` — `shutdown()` stops all
/// of a node's services. Releases outside the registry lock: dropping a
/// registration can take its own locks.
pub(crate) fn release_all(node: &Arc<MeshNode>) {
    let held = registry().lock().remove(&key_of(node));
    for entry in held.into_iter().flat_map(HashMap::into_values) {
        entry.release();
    }
}
