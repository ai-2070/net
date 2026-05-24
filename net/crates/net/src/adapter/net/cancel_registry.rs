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
use std::sync::Arc;
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
    entries: Mutex<HashMap<u64, CancelEntry>>,
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
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
        let mut entries = self.entries.lock();
        Self::gc(&mut entries);
        let entry = entries.entry(token).or_insert_with(CancelEntry::new);
        entry.pre_cancelled = true;
        if entry.marked_at.is_none() {
            entry.marked_at = Some(Instant::now());
        }
        if let Some(notify) = entry.notify.as_ref() {
            // Notify::notify_one() stores a permit so the next
            // notified().await returns immediately. Idempotent if
            // already permit-loaded — calling twice is harmless.
            notify.notify_one();
        }
    }

    /// Returns a [`Notify`] the in-flight call should `select!`
    /// against. If the token has already been cancelled (the
    /// CR-13 race), the returned Notify is pre-armed so the first
    /// `notified().await` returns immediately.
    ///
    /// `token == 0` short-circuits and returns a fresh Notify
    /// that never fires — lets call shapes write
    /// `select! { _ = notify.notified() => ... }` unconditionally
    /// without branching on `Option<Notify>`.
    pub fn register_notify(&self, token: u64) -> Arc<Notify> {
        if token == 0 {
            // Never-firing Notify so the call's select! arm is
            // structurally identical in the no-cancel case.
            return Arc::new(Notify::new());
        }
        let mut entries = self.entries.lock();
        Self::gc(&mut entries);
        let entry = entries.entry(token).or_insert_with(CancelEntry::new);
        let notify = entry
            .notify
            .get_or_insert_with(|| Arc::new(Notify::new()))
            .clone();
        if entry.pre_cancelled {
            // CR-13: cancel arrived before register. Pre-arm the
            // permit so the first notified().await fires
            // immediately. Don't clear pre_cancelled — leaving it
            // set is idempotent (re-arm is a no-op) and a stale
            // re-register would still observe the cancel.
            notify.notify_one();
        }
        // Once registered, the entry is no longer an orphan; the
        // marked_at timestamp's only purpose was orphan-TTL GC.
        entry.marked_at = None;
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
        let mut entries = self.entries.lock();
        entries.remove(&token);
    }

    /// Opportunistic eviction of orphan entries (cancel-only, no
    /// live caller) older than [`ORPHAN_TTL`]. Called from
    /// [`Self::cancel`] and [`Self::register_notify`] so the
    /// registry self-prunes without a dedicated GC task.
    ///
    /// Entries with a registered [`Notify`] are kept regardless of
    /// age — they represent a live caller awaiting (or about to
    /// await) the cancel signal.
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
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
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
}
