//! `ClaimRegistry` — which daemon currently holds which exclusive claim.
//!
//! The step-driver records a claim here on `StepGate::Running(claim)`
//! and clears it on `release_step`; the bridge projections only *read*
//! it. Keyed by `DaemonRef` (not `TaskId`) because the consumers ask a
//! daemon-keyed question: Projection 2 folds over `(daemon, claim)` to
//! pin placement, and the Projection-5 migration veto asks
//! `holds_exclusive(&daemon)`. The `daemon_ref` encoding is one-way
//! (`splitmix64`), so a `TaskId` key could not answer that — the
//! `DaemonRef` is the durable handle both projections share.

use std::collections::HashMap;

use crate::adapter::net::behavior::meshos::DaemonRef;
use crate::adapter::net::cortex::workflow::ActiveClaim;

/// In-memory map of `DaemonRef -> ActiveClaim` for daemons currently
/// holding an exclusive island claim. The owner (step-driver / runtime)
/// mutates it as claims are acquired and released; the bridge
/// projections borrow it read-only.
#[derive(Debug, Clone, Default)]
pub struct ClaimRegistry {
    held: HashMap<DaemonRef, ActiveClaim>,
}

impl ClaimRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `daemon` now holds `claim` — call on
    /// `StepGate::Running(claim)`. Replaces any prior claim for the
    /// same daemon (a re-claim after release reuses the same ref).
    pub fn insert(&mut self, daemon: DaemonRef, claim: ActiveClaim) {
        self.held.insert(daemon, claim);
    }

    /// Drop `daemon`'s claim — call on `release_step`. Returns the
    /// released claim, if any. Idempotent: releasing a daemon that
    /// holds nothing is a no-op returning `None`.
    pub fn remove(&mut self, daemon: &DaemonRef) -> Option<ActiveClaim> {
        self.held.remove(daemon)
    }

    /// The claim `daemon` holds, if any.
    pub fn get(&self, daemon: &DaemonRef) -> Option<&ActiveClaim> {
        self.held.get(daemon)
    }

    /// Does `daemon` hold an exclusive claim? The Projection-5
    /// migration-veto predicate (`MigrationEligible::check`).
    pub fn holds_exclusive(&self, daemon: &DaemonRef) -> bool {
        self.held.contains_key(daemon)
    }

    /// Number of held claims.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// True when no claims are held.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Iterate over every `(daemon, claim)` currently held — the read
    /// Projection 2 folds over.
    pub fn iter(&self) -> impl Iterator<Item = (&DaemonRef, &ActiveClaim)> {
        self.held.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dref(id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: format!("task/{id}"),
        }
    }

    #[test]
    fn insert_get_holds_remove_roundtrip() {
        let mut reg = ClaimRegistry::new();
        let d = dref(1);
        assert!(!reg.holds_exclusive(&d));
        assert!(reg.is_empty());

        reg.insert(d.clone(), ActiveClaim { island: 0xA0 });
        assert!(reg.holds_exclusive(&d), "veto sees the claim once held");
        assert_eq!(reg.get(&d).map(|c| c.island), Some(0xA0));
        assert_eq!(reg.len(), 1);

        let released = reg.remove(&d);
        assert_eq!(released.map(|c| c.island), Some(0xA0));
        assert!(
            !reg.holds_exclusive(&d),
            "veto sees no claim after release — daemon becomes migratable",
        );
        assert!(reg.remove(&d).is_none(), "release is idempotent");
    }

    #[test]
    fn distinct_daemons_hold_independent_claims() {
        let mut reg = ClaimRegistry::new();
        reg.insert(dref(1), ActiveClaim { island: 0xA0 });
        reg.insert(dref(2), ActiveClaim { island: 0xB0 });
        assert_eq!(reg.get(&dref(1)).map(|c| c.island), Some(0xA0));
        assert_eq!(reg.get(&dref(2)).map(|c| c.island), Some(0xB0));
        // Releasing one leaves the other untouched.
        reg.remove(&dref(1));
        assert!(!reg.holds_exclusive(&dref(1)));
        assert!(reg.holds_exclusive(&dref(2)));
    }
}
