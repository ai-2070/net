//! Projection 5 — the migration veto, enforced by type (not convention).
//!
//! An exclusive `ActiveClaim` pins its daemon for the claim's lifetime:
//! migrating compute off the node whose island it holds is either a
//! double-book or an impossible re-claim (plan Locked Decision 2). The
//! veto is enforced so it cannot be bypassed by forgetting a check —
//! the migration entry point [`migrate`] accepts only a
//! [`MigrationEligible`] token, and the only way to build that token is
//! [`MigrationEligible::check`], which consults the [`ClaimRegistry`].
//! A code path that tries to migrate a claim-holder simply does not
//! type-check (plan LD 4): the bypass is a type error, not a forgotten
//! `can_migrate()` call.
//!
//! Drain of a claim-holder is therefore `release → re-claim → restart`,
//! never live migration: releasing the claim clears the registry (so
//! `check` then succeeds), and the destination island is acquired by an
//! ordinary Thunderdome re-claim (plan Resolved Decision 4).

use std::fmt;

use crate::adapter::net::behavior::meshos::{DaemonRef, NodeId};

use super::claim_registry::ClaimRegistry;

/// Returned by [`MigrationEligible::check`] when the daemon still holds
/// an exclusive claim and therefore must not be migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimHeld(pub DaemonRef);

impl fmt::Display for ClaimHeld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "daemon {:?} holds an exclusive claim and cannot be migrated; \
             drain is release -> re-claim -> restart",
            self.0
        )
    }
}

impl std::error::Error for ClaimHeld {}

/// Proof that a daemon is *not* a claim-holder and may be migrated. The
/// inner `DaemonRef` is private and there is no other constructor, so a
/// value of this type can only come from [`MigrationEligible::check`]
/// passing the claim check — the type itself *is* the veto.
#[derive(Debug, Clone)]
pub struct MigrationEligible(DaemonRef);

impl MigrationEligible {
    /// Prove `daemon` may be migrated by checking it holds no exclusive
    /// claim. Returns [`ClaimHeld`] (the veto) if it does.
    pub fn check(daemon: DaemonRef, claims: &ClaimRegistry) -> Result<Self, ClaimHeld> {
        if claims.holds_exclusive(&daemon) {
            return Err(ClaimHeld(daemon));
        }
        Ok(MigrationEligible(daemon))
    }

    /// The daemon proven eligible — read by the executor after the gate.
    pub fn daemon(&self) -> &DaemonRef {
        &self.0
    }
}

/// A validated migration command: a daemon proven claim-free plus its
/// destination node. The only way to obtain one is [`migrate`], which
/// consumes a [`MigrationEligible`], so a `MigrationPlan` can never name
/// a claim-holder — the property the MeshOS migration executor relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    /// The daemon to move — proven not to hold an exclusive claim.
    pub daemon: DaemonRef,
    /// The destination node.
    pub target: NodeId,
}

/// The single migration entry point. Consumes the eligibility proof and
/// produces the [`MigrationPlan`] the MeshOS migration executor runs.
/// Because it takes [`MigrationEligible`] by value — constructible only
/// via [`MigrationEligible::check`] — a claim-holder can never reach a
/// migration: the bypass is a type error, not a forgotten call (LD 4).
///
/// The veto is mechanical: a bare `DaemonRef` does not type-check here.
/// ```compile_fail
/// # use net::adapter::net::behavior::scheduler_bridge::migrate;
/// # use net::adapter::net::behavior::meshos::DaemonRef;
/// let daemon = DaemonRef { id: 1, name: "task/1".into() };
/// // `migrate` requires a `MigrationEligible`; a claim-unchecked
/// // `DaemonRef` does not type-check — this is the veto.
/// let _ = migrate(daemon, 42);
/// ```
pub fn migrate(eligible: MigrationEligible, target: NodeId) -> MigrationPlan {
    MigrationPlan {
        daemon: eligible.0,
        target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::cortex::workflow::ActiveClaim;

    fn dref(id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: format!("task/{id}"),
        }
    }

    #[test]
    fn a_claim_free_daemon_is_eligible_and_migrates() {
        let claims = ClaimRegistry::new();
        let d = dref(1);
        let eligible =
            MigrationEligible::check(d.clone(), &claims).expect("no claim held → eligible");
        assert_eq!(eligible.daemon(), &d);
        let plan = migrate(eligible, 42);
        assert_eq!(
            plan,
            MigrationPlan {
                daemon: d,
                target: 42,
            }
        );
    }

    #[test]
    fn a_claim_holder_is_vetoed() {
        let mut claims = ClaimRegistry::new();
        let d = dref(1);
        claims.insert(d.clone(), ActiveClaim { island: 0xA0 });
        let err =
            MigrationEligible::check(d.clone(), &claims).expect_err("claim-holder must be vetoed");
        assert_eq!(err, ClaimHeld(d));
    }

    #[test]
    fn releasing_the_claim_makes_the_daemon_eligible_again() {
        // Drain is release → re-claim → restart: once the claim is
        // released the daemon becomes migratable again (the destination
        // is then acquired by an ordinary re-claim, never double-held).
        let mut claims = ClaimRegistry::new();
        let d = dref(1);
        claims.insert(d.clone(), ActiveClaim { island: 0xA0 });
        assert!(MigrationEligible::check(d.clone(), &claims).is_err());

        claims.remove(&d);
        assert!(
            MigrationEligible::check(d.clone(), &claims).is_ok(),
            "after release the veto lifts",
        );
    }

    #[test]
    fn the_veto_is_per_daemon_not_global() {
        // A held claim on one daemon does not block migrating another.
        let mut claims = ClaimRegistry::new();
        let held = dref(1);
        let free = dref(2);
        claims.insert(held.clone(), ActiveClaim { island: 0xA0 });
        assert!(MigrationEligible::check(held, &claims).is_err());
        assert!(MigrationEligible::check(free, &claims).is_ok());
    }
}
