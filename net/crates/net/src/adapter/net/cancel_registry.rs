//! Substrate-level cancel-token registry for the nRPC call shapes.
//!
//! Promotes the cancel-token pattern that the napi binding owned
//! at `bindings/node/src/mesh_rpc.rs` to the SDK layer so all three
//! bindings (and any future direct-consumer of [`crate::adapter::net::MeshNode`])
//! get cancel semantics through a single primitive.
//!
//! # Model
//!
//! Each in-flight call optionally carries a `cancel_token: u64`
//! reserved via [`crate::adapter::net::MeshNode::reserve_cancel_token`].
//! A caller signals cancellation from any thread by calling
//! [`crate::adapter::net::MeshNode::cancel`] with that token. The
//! in-flight call's await point observes the cancel via a
//! [`tokio::sync::Notify`] permit and short-circuits to
//! [`crate::adapter::net::mesh_rpc::RpcError::Cancelled`].
//!
//! Drop-on-cancel emits CANCEL on the wire via the existing
//! [`crate::adapter::net::mesh_rpc`]-side guards (UnaryCallGuard,
//! ClientStreamCallRaw::Drop, DuplexCallRaw::Drop).
//!
//! # Race fixes
//!
//! Two races the napi binding's local registry already pinned:
//!
//! 1. **Cancel-before-register.** A call's `cancel(token)` can land
//!    BEFORE the call's `register_cancel_notify(token)` runs (the
//!    gap between caller-side token reservation and the in-flight
//!    call reaching its `select!`). The registry latches a
//!    `pre_cancelled = true` flag on the orphan entry; the
//!    subsequent register observes the flag and the returned
//!    [`Notify`] is pre-armed via [`Notify::notify_one`] so the
//!    first `notified().await` returns immediately. Matches the
//!    napi binding's CR-13 fix.
//!
//! 2. **Orphaned cancel-only entries.** A pathological caller that
//!    reserves a token, calls cancel, and then never issues the
//!    paired call leaks an entry in the registry forever. An
//!    opportunistic GC on every `cancel(token)` evicts orphan
//!    entries older than [`ORPHAN_TTL`] (120s, matching the Go
//!    FFI's Q18 fix). The registry is a single HashMap, not on
//!    any hot path, so the per-call GC scan cost is irrelevant.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;

/// How long an orphaned (cancel-arrived-first, no live call yet)
/// registry entry stays before opportunistic GC evicts it. The
/// window is long enough for a legitimate `reserve → slow
/// dispatch → register` flow to still observe the cancellation,
/// but short enough that a misbehaving caller can't grow the
/// registry unboundedly.
///
/// Tuned to match the Go FFI's Q18 fix value. See
/// `bindings/go/rpc-ffi/src/lib.rs::ORPHAN_TTL`.
pub const ORPHAN_TTL: Duration = Duration::from_secs(120);

/// Process-global counter for cancel tokens. Starts at 1 so `0`
/// can be a "no token" sentinel that
/// [`crate::adapter::net::MeshNode::cancel`] ignores. Monotonically-
/// increasing, never reused.
static NEXT_CANCEL_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Shared never-firing [`Notify`] returned by [`CancelRegistry::register_notify`]
/// when `token == 0`. Every no-cancel call (the overwhelmingly common
/// path) clones this single `Arc` instead of allocating a fresh
/// `Notify` per call — saves two heap allocations + two refcount ops
/// on the unary/streaming hot paths.
///
/// Safe to share: nothing ever calls `notify_one` on it, so the
/// permit-slot stays empty forever and every `notified().await`
/// blocks indefinitely (which is the semantic we want — the call's
/// `tokio::select!` arm fires only on the other branch).
fn never_firing_notify() -> Arc<Notify> {
    static NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
    NOTIFY.get_or_init(|| Arc::new(Notify::new())).clone()
}

/// Minimum interval between opportunistic GC sweeps. Called from
/// the hot path ([`CancelRegistry::register_notify`]), so a full
/// HashMap scan per call is O(N) under contention. Rate-limiting
/// to once per second keeps orphan-TTL eviction bounded without
/// the quadratic burst behavior.
const GC_INTERVAL: Duration = Duration::from_secs(1);

/// Reserve a fresh cancel token. Process-global counter — every
/// [`crate::adapter::net::MeshNode`] in the process shares the same
/// sequence so a downstream consumer holding multiple meshes can
/// build a single cancel-routing layer without collision concerns.
pub(crate) fn next_token() -> u64 {
    NEXT_CANCEL_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Per-token state. Carries the cancel signal and a generation
/// marker for orphan-TTL GC.
struct CancelEntry {
    /// Set when [`CancelRegistry::cancel`] arrived BEFORE the
    /// matching `register_cancel_notify` ran. The first register
    /// observes the flag, immediately arms the returned `Notify`,
    /// and clears the flag.
    pre_cancelled: bool,
    /// The [`Notify`] the in-flight call awaits. `None` when only
    /// `pre_cancelled` is set (cancel-before-register). Populated
    /// on first register; subsequent registers (if any — typical
    /// use is 1:1) clone the existing Arc.
    notify: Option<Arc<Notify>>,
    /// When the entry was created. Used by the orphan-TTL GC.
    /// Set on cancel-before-register (so unused tokens can age
    /// out); cleared after register since a registered entry has
    /// a live caller awaiting.
    marked_at: Option<Instant>,
}

impl CancelEntry {
    fn new() -> Self {
        Self {
            pre_cancelled: false,
            notify: None,
            marked_at: None,
        }
    }
}

/// Per-mesh cancel-token registry. Lives behind an Arc on
/// [`crate::adapter::net::MeshNode`].
///
/// All public surfaces ([`Self::reserve_token`], [`Self::cancel`],
/// [`Self::register_notify`], [`Self::release`]) are thread-safe
/// and cheap on the no-cancel path (no allocation when
/// `token == 0`).
pub struct CancelRegistry {
    entries: Mutex<RegistryInner>,
}

struct RegistryInner {
    entries: HashMap<u64, CancelEntry>,
    /// Last time [`CancelRegistry::gc`] swept the map. Rate-limits
    /// the sweep to once per [`GC_INTERVAL`] — the hot path callers
    /// (`register_notify`) check this before paying the O(N) scan.
    last_gc: Instant,
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(RegistryInner {
                entries: HashMap::new(),
                last_gc: Instant::now(),
            }),
        }
    }

    /// Reserve a fresh cancel token. Wraps the process-global
    /// counter from [`next_token`] — exposed as a method so
    /// downstream code uses the `mesh.reserve_cancel_token()`
    /// surface uniformly.
    pub fn reserve_token(&self) -> u64 {
        next_token()
    }

    /// Signal cancellation for `token`. Idempotent; safe to call
    /// from any thread. No-op when `token == 0` (the "no token"
    /// sentinel) or when no caller ever reserved this id.
    ///
    /// If the matching call has registered its [`Notify`], this
    /// arms it via [`Notify::notify_one`] so the call's `select!`
    /// arm fires. If the matching call hasn't registered yet
    /// (cancel-before-register race), this latches
    /// `pre_cancelled = true` on the orphan entry; the eventual
    /// register observes the flag and arms the returned Notify
    /// immediately.
    pub fn cancel(&self, token: u64) {
        if token == 0 {
            return;
        }
        let notify = {
            let mut inner = self.entries.lock();
            // Cancel rate is low and bounded, so GC under cancel
            // stays cheap; still gate by `last_gc` to stay
            // symmetric with register_notify.
            Self::maybe_gc(&mut inner);
            let entry = inner.entries.entry(token).or_insert_with(CancelEntry::new);
            entry.pre_cancelled = true;
            if entry.marked_at.is_none() {
                entry.marked_at = Some(Instant::now());
            }
            // Clone the Arc before releasing the lock — `notify_one`
            // doesn't need the registry mutex.
            entry.notify.clone()
        };
        if let Some(notify) = notify {
            notify.notify_one();
        }
    }

    /// Returns a [`Notify`] the in-flight call should `select!`
    /// against. If the token has already been cancelled (the
    /// CR-13 race), the returned Notify is pre-armed so the first
    /// `notified().await` returns immediately.
    ///
    /// `token == 0` short-circuits to a shared never-firing Notify
    /// (one Arc clone, no allocation) so call shapes can write
    /// `select! { _ = notify.notified() => ... }` unconditionally
    /// without branching on `Option<Notify>` and without paying an
    /// allocation per call on the no-cancel hot path.
    pub fn register_notify(&self, token: u64) -> Arc<Notify> {
        if token == 0 {
            return never_firing_notify();
        }
        // Snapshot (notify, was_precancelled) under the lock, then
        // drop the guard before calling `notify_one`. Keeps the
        // critical section to a HashMap lookup + Arc clone.
        let (notify, was_precancelled) = {
            let mut inner = self.entries.lock();
            Self::maybe_gc(&mut inner);
            let entry = inner.entries.entry(token).or_insert_with(CancelEntry::new);
            let notify = entry
                .notify
                .get_or_insert_with(|| Arc::new(Notify::new()))
                .clone();
            let was_precancelled = entry.pre_cancelled;
            // Once registered, the entry is no longer an orphan; the
            // marked_at timestamp's only purpose was orphan-TTL GC.
            entry.marked_at = None;
            (notify, was_precancelled)
        };
        if was_precancelled {
            // CR-13: cancel arrived before register. Pre-arm the
            // permit so the first notified().await fires
            // immediately. Calling outside the lock avoids holding
            // the registry mutex across tokio internals.
            notify.notify_one();
        }
        notify
    }

    /// Remove a token's entry from the registry. Called by the
    /// in-flight call shape once it has resolved (success, error,
    /// or terminal cancel) so the registry doesn't grow
    /// unboundedly across long-running consumers.
    ///
    /// Idempotent — repeated calls on the same token are no-ops.
    /// `token == 0` is also a no-op.
    pub fn release(&self, token: u64) {
        if token == 0 {
            return;
        }
        let mut inner = self.entries.lock();
        inner.entries.remove(&token);
    }

    /// Rate-limited wrapper around [`Self::gc`]. Skips the O(N)
    /// scan if it ran within the last [`GC_INTERVAL`] — the hot
    /// path (`register_notify`) calls this on every register, so
    /// without rate-limiting a burst of N concurrent calls would
    /// pay O(N²) total scan cost.
    fn maybe_gc(inner: &mut RegistryInner) {
        let now = Instant::now();
        if now.duration_since(inner.last_gc) < GC_INTERVAL {
            return;
        }
        inner.last_gc = now;
        Self::gc(&mut inner.entries);
    }

    /// Opportunistic eviction of orphan entries (cancel-only, no
    /// live caller) older than [`ORPHAN_TTL`]. Entries with a
    /// registered [`Notify`] are kept regardless of age — they
    /// represent a live caller awaiting (or about to await) the
    /// cancel signal.
    fn gc(entries: &mut HashMap<u64, CancelEntry>) {
        let now = Instant::now();
        entries.retain(|_, entry| {
            if entry.notify.is_some() {
                return true;
            }
            match entry.marked_at {
                Some(t) => now.duration_since(t) < ORPHAN_TTL,
                None => true,
            }
        });
    }

    /// Number of entries currently tracked. Diagnostic; not on
    /// any hot path. Includes both registered-and-live entries
    /// and orphan cancel-only entries that haven't aged out yet.
    pub fn len(&self) -> usize {
        self.entries.lock().entries.len()
    }

    /// True iff the registry tracks zero entries. Mirrors the
    /// [`Vec::is_empty`] convention so clippy doesn't pester
    /// downstream consumers.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_zero_token_is_noop() {
        let reg = CancelRegistry::new();
        reg.cancel(0);
        assert_eq!(reg.len(), 0, "cancel(0) must not create an entry");
    }

    #[test]
    fn register_zero_token_returns_never_firing_notify() {
        let reg = CancelRegistry::new();
        let notify = reg.register_notify(0);
        // No permit pre-loaded, no entry inserted.
        assert_eq!(reg.len(), 0);
        // notify is a fresh Notify; we don't poll it (no permits)
        // but its existence lets call shapes select! against it
        // unconditionally.
        let _ = notify;
    }

    #[test]
    fn release_zero_token_is_noop() {
        let reg = CancelRegistry::new();
        reg.release(0);
        assert_eq!(reg.len(), 0);
    }

    #[tokio::test]
    async fn cancel_then_register_pre_arms_notify() {
        // The CR-13 race: a cancel that arrives BEFORE the call's
        // register call. The returned Notify must be pre-armed so
        // the first notified().await fires immediately.
        let reg = CancelRegistry::new();
        let token = reg.reserve_token();
        reg.cancel(token);
        let notify = reg.register_notify(token);
        // notified() returns immediately because notify_one was
        // already called before we arrived.
        tokio::time::timeout(Duration::from_millis(100), notify.notified())
            .await
            .expect("pre-armed Notify must fire immediately");
    }

    #[tokio::test]
    async fn register_then_cancel_wakes_waiter() {
        // The forward-direction: register first, then cancel.
        let reg = CancelRegistry::new();
        let token = reg.reserve_token();
        let notify = reg.register_notify(token);
        let reg2 = std::sync::Arc::new(reg);
        let reg2_clone = reg2.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            reg2_clone.cancel(token);
        });
        tokio::time::timeout(Duration::from_millis(500), notify.notified())
            .await
            .expect("register-then-cancel must wake the waiter");
    }

    #[test]
    fn release_removes_entry() {
        let reg = CancelRegistry::new();
        let token = reg.reserve_token();
        let _notify = reg.register_notify(token);
        assert_eq!(reg.len(), 1);
        reg.release(token);
        assert_eq!(reg.len(), 0);
        // Idempotent.
        reg.release(token);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn cancel_after_release_is_safe() {
        // Race: cancel arrives after the call has already
        // resolved + released. Should be a clean no-op (the
        // entry is gone), not a panic or double-counted action.
        let reg = CancelRegistry::new();
        let token = reg.reserve_token();
        let _notify = reg.register_notify(token);
        reg.release(token);
        reg.cancel(token);
        // A new orphan entry was created by the post-release
        // cancel. That's fine — the orphan-TTL GC will evict it.
        // The contract is that `cancel` is safe to call at any
        // time, not that it's a no-op on stale tokens.
        assert!(reg.len() <= 1);
    }

    #[test]
    fn next_token_is_monotonic_and_nonzero() {
        let a = next_token();
        let b = next_token();
        let c = next_token();
        assert!(a >= 1, "tokens start at 1, not 0");
        assert!(b > a);
        assert!(c > b);
    }

    /// N1: `register_notify(0)` returns the same process-wide
    /// `Arc<Notify>` on every call — no per-call allocation. The
    /// returned Arc's strong-count grows with each clone instead
    /// of starting fresh from 1. Pinned because the hot path's
    /// allocator pressure is the whole motivation for the cache.
    #[test]
    fn zero_token_returns_shared_never_firing_notify() {
        let reg = CancelRegistry::new();
        let a = reg.register_notify(0);
        let b = reg.register_notify(0);
        assert!(
            Arc::ptr_eq(&a, &b),
            "both no-cancel registrations must hand back the same Arc<Notify>"
        );
        // The shared Arc is also the static one, so a third clone
        // from never_firing_notify() matches.
        let c = never_firing_notify();
        assert!(Arc::ptr_eq(&a, &c));
        // Zero-token registrations don't create registry entries.
        assert_eq!(reg.len(), 0);
    }

    /// N2: `maybe_gc` skips the O(N) scan if it ran within
    /// [`GC_INTERVAL`]. A burst of register_notify calls touches
    /// `last_gc` exactly once, regardless of N. Pinned because
    /// the per-call quadratic burst was the original bug.
    #[test]
    fn gc_rate_limited_across_burst() {
        let reg = CancelRegistry::new();
        // Stamp `last_gc` to "just now" so any burst inside the
        // GC window must short-circuit.
        {
            let mut inner = reg.entries.lock();
            inner.last_gc = Instant::now();
        }
        // Manually insert an orphan entry that would normally be
        // collected by GC (ORPHAN_TTL = 120s; we stamp it as if it
        // had aged out). If GC fires on the next register call,
        // the entry vanishes.
        let stale = next_token();
        {
            let mut inner = reg.entries.lock();
            let entry = inner.entries.entry(stale).or_insert_with(CancelEntry::new);
            entry.pre_cancelled = true;
            entry.marked_at = Some(Instant::now() - (ORPHAN_TTL * 2));
        }
        // Trigger register_notify on a fresh token — should NOT
        // evict the stale entry because GC is rate-limited.
        let _ = reg.register_notify(next_token());
        assert!(
            reg.entries.lock().entries.contains_key(&stale),
            "stale entry survives because gc is rate-limited"
        );
    }

    /// N3: notify_one fires after the lock is released. Hard to
    /// observe directly without instrumenting parking_lot, but we
    /// can pin the contract by exercising the CR-13 pre-arm path
    /// (which goes through the same code) and asserting it still
    /// works.
    #[tokio::test]
    async fn pre_arm_works_with_lock_released_notify() {
        let reg = CancelRegistry::new();
        let token = reg.reserve_token();
        reg.cancel(token);
        let notify = reg.register_notify(token);
        tokio::time::timeout(Duration::from_millis(100), notify.notified())
            .await
            .expect("pre-armed Notify fires even with lock-narrowed register");
    }
}
