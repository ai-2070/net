//! Registry mapping daemon `origin_hash` to the pieces needed to restore
//! the daemon from a snapshot: a constructor closure, the matching
//! `EntityKeypair`, and a `DaemonHostConfig`.
//!
//! `MigrationTargetHandler::restore_snapshot` takes a
//! `daemon_factory: FnOnce() -> Box<dyn MeshDaemon>` plus a keypair and
//! config. These cannot be serialized across the wire, so the subprotocol
//! handler has to resolve them locally when a snapshot arrives. This
//! registry is that local resolver.
//!
//! Populate the registry at node startup with one entry per daemon type
//! the node may be asked to host.
//!
//! # Keypair provisioning (out of scope here)
//!
//! Secure transfer of a daemon's `EntityKeypair` from source to target is a
//! separate security problem. For now, callers provision the keypair in the
//! factory registry out-of-band (same shape the existing integration tests
//! use).

use std::sync::Arc;

use dashmap::DashMap;

use super::daemon::{DaemonError, DaemonHostConfig, MeshDaemon};
use crate::adapter::net::identity::EntityKeypair;

/// Bundle required to reconstruct a daemon on the target.
pub struct FactoryEntry {
    /// Constructor for a fresh, unrestored daemon instance.
    pub factory: Box<dyn Fn() -> Box<dyn MeshDaemon> + Send + Sync>,
    /// The daemon's signing keypair.
    ///
    /// - `Some(kp)` — caller pre-provisioned the keypair out-of-band.
    ///   Used as the default at restore; the dispatcher's envelope
    ///   path can still override when the snapshot carries one.
    /// - `None` — placeholder registration. The caller expects the
    ///   `IdentityEnvelope` to supply the keypair at restore time;
    ///   if the snapshot arrives without an envelope, restore fails
    ///   cleanly rather than silently synthesizing a wrong keypair.
    pub keypair: Option<EntityKeypair>,
    /// Host configuration to apply to the restored daemon.
    pub config: DaemonHostConfig,
}

/// Freshly built inputs for a single restore attempt. Produced by
/// [`DaemonFactoryRegistry::construct`] so the caller can retry the
/// restore on transient failures without losing the registration.
pub struct ConstructedInputs {
    /// A fresh daemon instance — unrestored state.
    pub daemon: Box<dyn MeshDaemon>,
    /// The daemon's signing keypair, when the factory was registered
    /// with one. `None` for placeholder registrations — the dispatcher
    /// must recover the real keypair from the snapshot's
    /// [`IdentityEnvelope`](crate::adapter::net::identity::IdentityEnvelope).
    pub keypair: Option<EntityKeypair>,
    /// Host configuration.
    pub config: DaemonHostConfig,
}

/// Thread-safe registry of daemon factories keyed by `origin_hash`.
#[derive(Default)]
pub struct DaemonFactoryRegistry {
    entries: DashMap<u64, FactoryEntry>,
}

impl DaemonFactoryRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory for a daemon type with a known keypair.
    ///
    /// The registration key is derived from `keypair.origin_hash()`; the
    /// caller does not supply it separately. Taking `origin_hash` as an
    /// argument used to invite a class of bugs where the caller passed a
    /// stale or unrelated value — now impossible by construction.
    ///
    /// Use this when the caller genuinely has the daemon's keypair
    /// in hand (local spawn, identity-transport opt-out). For the
    /// common envelope-transport case, where the keypair will arrive
    /// with the snapshot, prefer
    /// [`Self::register_placeholder`] — there's no reason to make up
    /// a fake keypair for the target.
    ///
    /// Returns [`DaemonError::ProcessFailed`] when the `origin_hash`
    /// already has an entry (live or
    /// placeholder). Callers that intend to replace an entry must
    /// [`Self::remove`] first. Insertion is atomic on collision —
    /// an existing entry is never clobbered, so a failed register
    /// does not corrupt state for the daemon that owns the slot.
    pub fn register<F>(
        &self,
        keypair: EntityKeypair,
        config: DaemonHostConfig,
        factory: F,
    ) -> Result<(), DaemonError>
    where
        F: Fn() -> Box<dyn MeshDaemon> + Send + Sync + 'static,
    {
        let origin_hash = keypair.origin_hash();
        match self.entries.entry(origin_hash) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(DaemonError::ProcessFailed(format!(
                "factory for origin_hash {origin_hash:#x} already registered"
            ))),
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(FactoryEntry {
                    factory: Box::new(factory),
                    keypair: Some(keypair),
                    config,
                });
                Ok(())
            }
        }
    }

    /// Register a placeholder factory keyed by `origin_hash` alone.
    /// No keypair is supplied — the dispatcher's target path will
    /// recover it from the migration snapshot's identity envelope.
    ///
    /// Use this on the target side of a migration that plans to
    /// transport identity via the envelope: the target legitimately
    /// doesn't know the daemon's private key ahead of time, and
    /// synthesizing a matching-origin keypair is cryptographically
    /// impossible. Restore without an envelope in the snapshot fails
    /// cleanly with an identity-transport error.
    ///
    /// Same collision semantics as [`Self::register`]: atomic fail
    /// on an already-registered `origin_hash`, never clobbers.
    pub fn register_placeholder<F>(
        &self,
        origin_hash: u64,
        config: DaemonHostConfig,
        factory: F,
    ) -> Result<(), DaemonError>
    where
        F: Fn() -> Box<dyn MeshDaemon> + Send + Sync + 'static,
    {
        match self.entries.entry(origin_hash) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(DaemonError::ProcessFailed(format!(
                "factory for origin_hash {origin_hash:#x} already registered"
            ))),
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(FactoryEntry {
                    factory: Box::new(factory),
                    keypair: None,
                    config,
                });
                Ok(())
            }
        }
    }

    /// Build fresh restore inputs (daemon instance + keypair + config) for
    /// `origin_hash` without removing the registration. The subprotocol
    /// handler uses this when it is about to attempt a restore but wants
    /// to retain the factory in case the attempt fails (e.g., transient
    /// snapshot parse error). Call [`Self::remove`] after a successful
    /// restore to mark the migration single-shot.
    pub fn construct(&self, origin_hash: u64) -> Option<ConstructedInputs> {
        let entry = self.entries.get(&origin_hash)?;
        Some(ConstructedInputs {
            daemon: (entry.factory)(),
            keypair: entry.keypair.clone(),
            config: entry.config.clone(),
        })
    }

    /// Remove the factory entry for `origin_hash` (e.g., after a
    /// successful restore). Idempotent: removing a non-existent entry is
    /// a no-op.
    pub fn remove(&self, origin_hash: u64) {
        self.entries.remove(&origin_hash);
    }

    /// Consume the factory entry for `origin_hash`, if any. Returns `None`
    /// when no factory has been registered.
    ///
    /// Prefer [`Self::construct`] + [`Self::remove`] in callers that want
    /// to retry restore on failure — `take` discards the entry even if the
    /// caller hasn't actually used it yet.
    pub fn take(&self, origin_hash: u64) -> Option<FactoryEntry> {
        self.entries.remove(&origin_hash).map(|(_, entry)| entry)
    }

    /// Whether a factory is currently registered for `origin_hash`.
    pub fn contains(&self, origin_hash: u64) -> bool {
        self.entries.contains_key(&origin_hash)
    }

    /// An empty shared registry, for handlers that are never expected to
    /// restore daemons (e.g., source-only nodes).
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl std::fmt::Debug for DaemonFactoryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonFactoryRegistry")
            .field("entries", &self.entries.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityFilter;
    use crate::adapter::net::compute::DaemonError;
    use crate::adapter::net::state::causal::CausalEvent;
    use bytes::Bytes;

    struct Stub;
    impl MeshDaemon for Stub {
        fn name(&self) -> &str {
            "stub"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
    }

    #[test]
    fn register_and_take_returns_entry_once() {
        let reg = DaemonFactoryRegistry::new();
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();

        reg.register(kp, DaemonHostConfig::default(), || Box::new(Stub))
            .unwrap();
        assert!(reg.contains(origin));

        let entry = reg.take(origin).expect("factory should be present");
        let _daemon = (entry.factory)();
        assert!(!reg.contains(origin), "take must consume the entry");
        assert!(reg.take(origin).is_none());
    }

    #[test]
    fn take_missing_returns_none() {
        let reg = DaemonFactoryRegistry::new();
        assert!(reg.take(0xDEADBEEF).is_none());
    }

    /// Regression: `register` used to take a separate `origin_hash`
    /// parameter and only `debug_assert_eq!` it against the keypair.
    /// Release builds silently accepted a mismatched keypair, which would
    /// later fail at `restore_snapshot` with a cryptic identity error —
    /// or, worse, register the daemon under the wrong identity.
    ///
    /// The fix is to derive `origin_hash` from the keypair: no caller can
    /// supply a stale or unrelated value. This test verifies the stored
    /// entry is always keyed by the keypair's own `origin_hash`.
    #[test]
    fn test_regression_register_always_uses_keypair_origin() {
        let reg = DaemonFactoryRegistry::new();
        let kp = EntityKeypair::generate();
        let expected = kp.origin_hash();

        reg.register(kp, DaemonHostConfig::default(), || Box::new(Stub))
            .unwrap();

        assert!(
            reg.contains(expected),
            "factory must be keyed by the keypair's origin_hash"
        );
        // No other origin_hash accepts the lookup — the previous API
        // allowed that when the caller passed a mismatched value.
        assert!(!reg.contains(expected.wrapping_add(1)));
    }

    /// Regression: factory inputs were consumed (via `take`) before
    /// `restore_snapshot` ran. A transient failure — e.g., a corrupted
    /// chunk that parsed to garbage — would discard the registration, so
    /// a retry could not find the factory. The caller would need to
    /// manually re-register before another migration could succeed.
    ///
    /// The fix is to expose `construct` for non-destructive access, and
    /// make `remove` a separate step that callers invoke only after a
    /// successful restore.
    #[test]
    fn test_regression_construct_preserves_entry_for_retry() {
        let reg = DaemonFactoryRegistry::new();
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();

        reg.register(kp, DaemonHostConfig::default(), || Box::new(Stub))
            .unwrap();

        let first = reg
            .construct(origin)
            .expect("first attempt should find factory");
        drop(first); // simulate restore failure

        // Retry must still find the factory.
        let second = reg
            .construct(origin)
            .expect("second attempt must still find factory after a failed first attempt");
        drop(second);

        // Explicit removal is single-step.
        reg.remove(origin);
        assert!(reg.construct(origin).is_none());
    }

    /// Regression: `register` used to `DashMap::insert` unconditionally,
    /// silently clobbering any existing entry. The SDK wrapped that in
    /// a rollback-on-host-collision path — so a second `spawn` with
    /// the same identity would overwrite the *first* daemon's factory
    /// entry, the subsequent `DaemonRegistry::register` would fail
    /// (correct), and the rollback would then strip the now-clobbered
    /// entry. Net result: the first daemon stayed live but lost its
    /// factory registration, which broke future migrations for it.
    ///
    /// Fix: `register` is atomic — collision returns an error and
    /// never touches the existing entry. A failed register gives the
    /// caller no ownership of the slot, so there is nothing to roll
    /// back.
    #[test]
    fn test_regression_register_fails_on_collision_without_clobbering() {
        let reg = DaemonFactoryRegistry::new();
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();

        // First register: the incumbent. Its factory emits a fixed
        // marker so we can tell it apart from any replacement.
        reg.register(kp.clone(), DaemonHostConfig::default(), || {
            Box::new(MarkerDaemon(0xA1))
        })
        .expect("first register");

        // Collision: second register with the same keypair must fail
        // cleanly. Pre-fix, this call silently replaced the entry.
        let err = reg
            .register(kp.clone(), DaemonHostConfig::default(), || {
                Box::new(MarkerDaemon(0xB2))
            })
            .expect_err("duplicate register must fail");
        assert!(
            matches!(err, DaemonError::ProcessFailed(ref m) if m.contains("already registered")),
            "expected ProcessFailed, got {err:?}",
        );

        // Incumbent survives: the factory still produces 0xA1, proving
        // we did not clobber. A construct() pulls a fresh instance and
        // we read its marker through the MeshDaemon::name() channel.
        let inputs = reg
            .construct(origin)
            .expect("incumbent factory must still be registered");
        assert_eq!(
            inputs.daemon.name(),
            "marker-0xa1",
            "duplicate-register must not replace the first daemon's factory"
        );
        // Incumbent is still migratable: a fresh `construct` after a
        // failed register should work. (Before the fix, the SDK's
        // rollback after `DaemonRegistry::register` collision would
        // strip the entry the *incumbent* was relying on.)
        let _again = reg.construct(origin).expect("still present");
    }

    /// Same atomic semantics for `register_placeholder` — a placeholder
    /// collision must not clobber the existing (placeholder or
    /// keypair-bearing) entry.
    #[test]
    fn test_regression_register_placeholder_fails_on_collision() {
        let reg = DaemonFactoryRegistry::new();
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();

        // Incumbent is a keypair-bearing entry (typical of a live
        // spawn). Placeholder collision must not downgrade it.
        reg.register(kp, DaemonHostConfig::default(), || {
            Box::new(MarkerDaemon(0xA1))
        })
        .expect("incumbent register");

        let err = reg
            .register_placeholder(origin, DaemonHostConfig::default(), || {
                Box::new(MarkerDaemon(0xB2))
            })
            .expect_err("placeholder collision must fail");
        assert!(
            matches!(err, DaemonError::ProcessFailed(ref m) if m.contains("already registered")),
            "expected ProcessFailed, got {err:?}",
        );

        // Incumbent retains its keypair — the placeholder branch would
        // have cleared it.
        let inputs = reg.construct(origin).expect("incumbent survives");
        assert!(
            inputs.keypair.is_some(),
            "keypair-bearing incumbent must not be downgraded to placeholder",
        );
    }

    struct MarkerDaemon(u8);
    impl MeshDaemon for MarkerDaemon {
        fn name(&self) -> &str {
            // The marker value rides in the name so `test_regression_*`
            // can distinguish the incumbent from any replacement
            // without threading a side-channel.
            match self.0 {
                0xA1 => "marker-0xa1",
                0xB2 => "marker-0xb2",
                _ => "marker-unknown",
            }
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
    }
}
