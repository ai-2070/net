//! The verify-time revocation view: what the admission engine
//! actually reads from core's filesystem-backed revocation store.
//!
//! Core's `org_revocation.rs` is NOT portable (a `std::fs`
//! interprocess store with `Condvar`s) and stays native. What the
//! admission engine reads at verify time is exactly one query — the
//! merged raise-only per-member floor maxima — plus the two store
//! health facts the §9.5 stability hook compares. The leaf is FED
//! those over its control channel as [`RevocationFacts`]; the merge
//! rule is ported verbatim: raise-only (a lower floor never rolls
//! back a higher one) and floor 0 is skipped (it is the "no floor"
//! default, not a revocation).

use std::collections::BTreeMap;

use crate::org::cert::OrgId;
use crate::org::entity::EntityId;

/// The revocation facts one admission verifies against — the leaf's
/// stand-in for core's `OrgRevocationState` snapshot.
///
/// Fed from the control plane (verified [`crate::org::cert::OrgRevocationBundle`]s
/// merged in via [`Self::merge_floors`]): merged raise-only maxima,
/// the publish epoch, and store health.
#[derive(Debug, Clone, Default)]
pub struct RevocationFacts {
    /// Merged raise-only maxima keyed `(org, member)`; a missing key
    /// means floor 0 — every generation admissible.
    pub floors: BTreeMap<(OrgId, EntityId), u32>,
    /// Publish epoch (core's `BarrieredGeneration::get`); bumps on
    /// every raise — `stability_recheck` uses it to detect "floor
    /// rose mid-admission".
    pub epoch: u64,
    /// Core's `OrgRevocationStore::is_poisoned()` — store health.
    /// `true` ⇒ deny `ProviderAuthorityUnavailable` / fail
    /// `stability_recheck`.
    pub poisoned: bool,
}

impl RevocationFacts {
    /// "Every cert `member` holds from `org` below this generation
    /// is revoked." Absent key ⇒ 0 ⇒ every generation admissible.
    pub fn floor_for(&self, org: &OrgId, member: &EntityId) -> u32 {
        self.floors
            .get(&(*org, member.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// Raise-only merge (mirrors core's
    /// `OrgRevocationState::merge_bundle`): floor 0 entries are
    /// skipped, a strictly-higher floor raises the stored maximum,
    /// lower floors never roll back. Returns the count of raised
    /// floors; the epoch bumps once if anything raised — the publish
    /// generation advances with every raise, which is exactly what
    /// `stability_recheck` watches.
    pub fn merge_floors(&mut self, org: OrgId, floors: &[(EntityId, u32)]) -> usize {
        let mut raised = 0;
        for (member, floor) in floors {
            if *floor == 0 {
                continue;
            }
            let entry = self.floors.entry((org, member.clone())).or_insert(0);
            if *floor > *entry {
                *entry = *floor;
                raised += 1;
            }
        }
        if raised > 0 {
            self.epoch = self.epoch.saturating_add(1);
        }
        raised
    }
}
